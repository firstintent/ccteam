//! Claude Code TUI-render base patterns ("high reliability" tier).
//!
//! Each entry's `id` is the `regex_id` that surfaces on
//! [`crate::MuxEvent::PatternMatched`] and joins onto the W4 dispatch
//! table. The regexes match against a single *completed* output line
//! (the subscribe translator splits on `\n` before calling the
//! matcher), with `String::from_utf8_lossy` already applied — so the
//! `●` / `⎿` glyphs are matched as their UTF-8 forms.
//!
//! Reliability notes per entry are inline. The `workspace` `regex`
//! crate is built `default-features = false` with only `std` + `perf`
//! (no `unicode`), so `\w` / `\b` / `\d` are ASCII-class — fine for
//! tool names and token counts, which are ASCII in Claude's TUI.

use super::PatternEntry;

/// The 10 high-reliability Claude TUI patterns.
///
/// Ordering is the §15.2 research order; `match_line` preserves it so
/// a single line that trips several patterns fires `regex_id`s
/// deterministically.
pub const CLAUDE_BASE_PATTERNS: &[PatternEntry] = &[
    // 1. Tool call started. Claude prints `● ToolName(args…)` when it
    //    invokes a tool. Capture group 1 = the tool name. The leading
    //    `●` glyph is the load-bearing anchor; `\s+` tolerates the
    //    one-or-more spaces Claude renders. HIGH reliability.
    PatternEntry {
        id: "tool_call_started",
        regex: r"^●\s+(\w+)\(",
    },
    // 2. Tool call completed. The result block opens with the `⎿`
    //    continuation glyph. We match the glyph anywhere a line starts
    //    with it (optionally indented). Whole-match capture (no group).
    //    HIGH reliability.
    PatternEntry {
        id: "tool_call_completed",
        regex: r"^\s*⎿",
    },
    // 3. Permission prompt. Claude asks "Do you want to allow/proceed"
    //    and renders a selectable list with a `❯` cursor / `[y]`
    //    affordance. We require the question phrasing AND a selection
    //    affordance so a mere mention of the words in assistant text
    //    doesn't false-fire. MEDIUM-HIGH reliability.
    PatternEntry {
        id: "permission_prompt",
        regex: r"Do you want to (?:allow|proceed).*(?:❯|>|\[y\])",
    },
    // 4. Rate limit. Anthropic surfaces "rate limit" / "too many
    //    requests" text on 429. Case-insensitive. HIGH reliability.
    PatternEntry {
        id: "rate_limit",
        regex: r"(?i)rate limit|too many requests",
    },
    // 5. Context overflow / compaction. Claude warns when context is
    //    low / running out, or announces a compact. Case-insensitive.
    //    MEDIUM-HIGH reliability.
    PatternEntry {
        id: "context_overflow",
        regex: r"(?i)context (?:low|left|window)|compact",
    },
    // 6. Token usage. The status line renders counts like "1.2k
    //    tokens" / "45000 tokens". Capture group 1 = the count token
    //    (incl. optional k/m suffix). Case-insensitive on the suffix.
    //    MEDIUM reliability (status-line format drifts across builds).
    PatternEntry {
        id: "token_usage",
        regex: r"(?i)(\d+(?:\.\d+)?[km]?)\s+tokens",
    },
    // 7. Thinking spinner. Claude cycles whimsical gerunds
    //    ("Thinking…", "Pondering…", "Cogitating…") next to the
    //    spinner. Kept deliberately loose — the ellipsis `…` alone is
    //    a decent signal. Case-insensitive. LOW-MEDIUM reliability
    //    (spinner vocabulary is large + changes); used only as an
    //    activity hint, never as an authoritative event.
    PatternEntry {
        id: "thinking",
        regex: r"(?i)\b(?:thinking|pondering|cogitating)\b|…",
    },
    // 8. User prompt submit echo. After the user submits, the TUI
    //    echoes the prompt with a leading `> `. Capture group 1 = the
    //    prompt text. MEDIUM reliability (the P1 Claude hook
    //    `user_prompt_submit` is authoritative; this is the lossy
    //    fallback).
    PatternEntry {
        id: "user_prompt_submit",
        regex: r"^> (.+)",
    },
    // 9. Session reset / welcome banner. `/new` and `/clear` redraw
    //    the welcome banner ("Welcome to Claude Code"). Matching the
    //    banner is the most stable reset signal that survives a render
    //    snapshot. MEDIUM reliability. The P1 hook (session_start) is
    //    authoritative.
    PatternEntry {
        id: "session_reset",
        regex: r"(?i)welcome to claude code",
    },
    // 10. Turn done — heuristic only.
    //
    //     LIMITATION: there is no single reliable "turn complete" line
    //     in the Claude TUI. The truthful signal is the absence of the
    //     spinner + the input prompt box being redrawn idle. A pure
    //     regex cannot observe "spinner gone"; that requires an
    //     idle-derived synthetic event (the subscribe translator's
    //     `OutputIdle` after N seconds of no output — see
    //     `MuxEvent::OutputIdle`). This pattern therefore matches the
    //     redrawn empty input box (`╰─` bottom border of the prompt
    //     box, or the bare prompt `> ` with nothing after it) as a
    //     best-effort proxy. Downstream MUST treat this as advisory and
    //     prefer the P1 Claude hook `stop` event when available. LOW
    //     reliability by design.
    PatternEntry {
        id: "turn_done",
        regex: r"^╰─|^>\s*$",
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patterns::{PatternMatcher, PatternVendor};
    use regex::Regex;

    /// Every base regex must compile — catches a table typo at test
    /// time rather than at the first `register_base_patterns` call in
    /// production.
    #[test]
    fn all_base_patterns_compile() {
        for entry in CLAUDE_BASE_PATTERNS {
            Regex::new(entry.regex)
                .unwrap_or_else(|e| panic!("pattern `{}` failed to compile: {e}", entry.id));
        }
    }

    /// There are exactly 10 entries (the §15.2 high-reliability tier);
    /// guards against an accidental drop/dup during edits.
    #[test]
    fn table_has_ten_entries_with_unique_ids() {
        assert_eq!(CLAUDE_BASE_PATTERNS.len(), 10);
        let mut ids: Vec<&str> = CLAUDE_BASE_PATTERNS.iter().map(|e| e.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 10, "regex_ids must be unique");
    }

    fn matcher() -> PatternMatcher {
        PatternMatcher::base(PatternVendor::Claude)
    }

    fn ids(hits: &[(String, String)]) -> Vec<&str> {
        hits.iter().map(|(id, _)| id.as_str()).collect()
    }

    #[test]
    fn tool_call_started_captures_tool_name() {
        let m = matcher();
        let hits = m.match_line("● Read(/foo/bar.rs)");
        assert!(ids(&hits).contains(&"tool_call_started"));
        let (_, captured) = hits
            .iter()
            .find(|(id, _)| id == "tool_call_started")
            .unwrap();
        assert_eq!(captured, "Read");
    }

    #[test]
    fn tool_call_completed_fires_on_continuation_glyph() {
        let m = matcher();
        let hits = m.match_line("  ⎿  Read 42 lines");
        assert!(ids(&hits).contains(&"tool_call_completed"));
    }

    #[test]
    fn permission_prompt_requires_affordance() {
        let m = matcher();
        // Has phrasing + cursor affordance → fires.
        let hit = m.match_line("Do you want to allow this edit? ❯ Yes");
        assert!(ids(&hit).contains(&"permission_prompt"));
        // Phrasing only, no affordance → does not fire.
        let no = m.match_line("I will ask: do you want to allow edits in general?");
        assert!(!ids(&no).contains(&"permission_prompt"));
    }

    #[test]
    fn rate_limit_case_insensitive() {
        let m = matcher();
        assert!(ids(&m.match_line("Error: Rate Limit exceeded")).contains(&"rate_limit"));
        assert!(ids(&m.match_line("429 too many requests")).contains(&"rate_limit"));
    }

    #[test]
    fn context_overflow_matches_compact_and_low() {
        let m = matcher();
        assert!(ids(&m.match_line("Context low — consider /compact")).contains(&"context_overflow"));
        assert!(ids(&m.match_line("Running compact now")).contains(&"context_overflow"));
    }

    #[test]
    fn token_usage_captures_count() {
        let m = matcher();
        let hits = m.match_line("Status: 1.2k tokens used");
        let (_, captured) = hits.iter().find(|(id, _)| id == "token_usage").unwrap();
        assert_eq!(captured, "1.2k");
    }

    #[test]
    fn user_prompt_submit_captures_prompt() {
        let m = matcher();
        let hits = m.match_line("> implement the login flow");
        let (_, captured) = hits
            .iter()
            .find(|(id, _)| id == "user_prompt_submit")
            .unwrap();
        assert_eq!(captured, "implement the login flow");
    }

    #[test]
    fn session_reset_matches_welcome_banner() {
        let m = matcher();
        assert!(ids(&m.match_line("✻ Welcome to Claude Code!")).contains(&"session_reset"));
    }

    #[test]
    fn turn_done_heuristic_matches_empty_prompt() {
        let m = matcher();
        assert!(ids(&m.match_line("> ")).contains(&"turn_done"));
        assert!(ids(&m.match_line("╰────────────────────╯")).contains(&"turn_done"));
    }

    #[test]
    fn plain_assistant_text_is_quiet() {
        let m = matcher();
        // A normal sentence should not trip the high-confidence
        // patterns (thinking's `…` is intentionally loose, so avoid it
        // here). tool/completed/permission/reset must stay quiet.
        let hits = m.match_line("Here is the summary of what I changed.");
        let fired = ids(&hits);
        assert!(!fired.contains(&"tool_call_started"));
        assert!(!fired.contains(&"tool_call_completed"));
        assert!(!fired.contains(&"permission_prompt"));
        assert!(!fired.contains(&"session_reset"));
    }
}
