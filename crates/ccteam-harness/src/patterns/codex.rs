//! Codex CLI TUI-render base patterns — deliberately a *thin* tier.
//!
//! Unlike Claude (whose richest signal is the Anthropic transcript +
//! Claude Code hook subprocess, with the TUI regexes a real fallback),
//! Codex exposes a **typed JSON-RPC channel** (`app-server` over UDS)
//! that is strictly richer than anything its TUI renders. See
//! `docs/versions/v0-8-rmux/w3b-codex-event-catalog.md` §6 — for nearly
//! every semantic event the answer to "can rmux PatternMatched
//! substitute?" is **no**: the typed notification carries decision
//! enums, ids, and breakdown tables that a regex on TUI bytes would
//! throw away.
//!
//! Therefore the Codex base table is the §6.4 "mode-3b L1/L2 safety
//! net" ONLY:
//!
//! - **L1 (process)** — process death / idle is owned by rmux process
//!   events (`ProcessExited`, `OutputIdle`), not regex; those are not
//!   table entries.
//! - **L2 (pattern)** — a *minimal* set of generic surface hints that
//!   stay useful when the UDS channel is unavailable (pre-`initialize`
//!   handshake, or an unsubscribed notification). Each entry below
//!   names the JSON-RPC notification that supersedes it once the bridge
//!   is live, so the W4 merger can treat the regex hit as a lossy P2
//!   fallback and prefer the typed P1 event.
//!
//! The W4 dispatch table (`crate::enriched_event`) still carries all of
//! the logical `EventKind` variants for Codex; a kind with no entry
//! here simply means "JSON-RPC only — no TUI fallback regex"
//! (`EnrichmentSource::CodexJsonRpc(..)` with the base never firing).

use super::PatternEntry;

/// Codex TUI base patterns — the minimal L2 safety-net tier.
///
/// Kept small on purpose (4 entries vs Claude's 10): Codex's typed
/// JSON-RPC notifications are canonical for tool calls, plan trees,
/// token usage, item lifecycle, and turn completion, so there is no
/// honest TUI regex for those — emitting one would be the §6.3
/// "actively wrong" anti-pattern (regex-parsing tool args / plan glyphs
/// / token counts off a TUI that truncates and rounds).
///
/// `match_line` preserves registration order, so a line tripping
/// several patterns fires `regex_id`s deterministically.
pub const CODEX_BASE_PATTERNS: &[PatternEntry] = &[
    // 1. Rate limit. SUPERSEDED BY JSON-RPC `account/rateLimits/updated`
    //    (typed `RateLimitSnapshot`). This regex is the fallback that
    //    still fires pre-`initialize` (before the bridge subscribes) or
    //    if the deny-list ever opts the notification out. Case-
    //    insensitive. Catalog §6.3 warns the TUI wording can drift, so
    //    the merger must treat this as P2-lossy. MEDIUM reliability.
    PatternEntry {
        id: "rate_limit",
        regex: r"(?i)rate limit|too many requests|429",
    },
    // 2. Thinking / working spinner. Codex renders a progress spinner
    //    with gerund-ish status text while a turn is in flight. SUPERSEDED
    //    BY JSON-RPC `turn/started` + `item/agentMessage/delta` (live
    //    activity is authoritative there). Pure activity hint, never an
    //    authoritative event — same low-reliability caveat as Claude's.
    //    Case-insensitive; the bare ellipsis `…` is a decent generic
    //    signal. LOW-MEDIUM reliability.
    PatternEntry {
        id: "thinking",
        regex: r"(?i)\b(?:thinking|working|esc to interrupt)\b|…",
    },
    // 3. Turn done — heuristic proxy. SUPERSEDED BY JSON-RPC
    //    `turn/completed` (the canonical turn boundary, carrying the
    //    usage breakdown). As with Claude there is no single reliable
    //    "turn complete" TUI line; we match the redrawn idle composer
    //    box (Codex prompts with a `▌`/`>` cursor on an otherwise empty
    //    input line). Downstream MUST prefer the typed event when the
    //    bridge is connected. LOW reliability by design.
    PatternEntry {
        id: "turn_done",
        regex: r"^\s*[▌>]\s*$",
    },
    // 4. Approval / permission prompt. SUPERSEDED BY the server-initiated
    //    JSON-RPC requests `item/commandExecution/requestApproval` /
    //    `item/fileChange/requestApproval` / `item/permissions/
    //    requestApproval` (catalog §2.2 / §4.5), which carry the full
    //    typed decision enum + proposed delta. This regex only flags
    //    that *some* approval gate is on screen when the UDS handler is
    //    absent — it cannot recover the decision options. We require the
    //    approval phrasing AND a selection affordance so plain assistant
    //    text mentioning "approve" doesn't false-fire. MEDIUM reliability.
    PatternEntry {
        id: "approval_prompt",
        regex: r"(?i)(?:allow|approve|run) (?:this )?command.*(?:❯|>|\[y\]|\(y/n\))",
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patterns::{PatternMatcher, PatternVendor};
    use regex::Regex;

    /// Every base regex must compile — catches a table typo at test
    /// time rather than at the first `register_base_patterns` call in
    /// production. Mirrors the Claude compile gate.
    #[test]
    fn all_base_patterns_compile() {
        for entry in CODEX_BASE_PATTERNS {
            Regex::new(entry.regex)
                .unwrap_or_else(|e| panic!("pattern `{}` failed to compile: {e}", entry.id));
        }
    }

    /// The Codex tier is intentionally small (the JSON-RPC channel is
    /// canonical). Guards against an accidental drop/dup, and documents
    /// the "do not fabricate 10 to mirror Claude" decision.
    #[test]
    fn table_is_small_with_unique_ids() {
        assert_eq!(CODEX_BASE_PATTERNS.len(), 4);
        let mut ids: Vec<&str> = CODEX_BASE_PATTERNS.iter().map(|e| e.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 4, "regex_ids must be unique");
    }

    fn matcher() -> PatternMatcher {
        PatternMatcher::base(PatternVendor::Codex)
    }

    fn ids(hits: &[(String, String)]) -> Vec<&str> {
        hits.iter().map(|(id, _)| id.as_str()).collect()
    }

    #[test]
    fn base_codex_matcher_loads_all_entries() {
        let m = matcher();
        assert_eq!(m.len(), CODEX_BASE_PATTERNS.len());
    }

    #[test]
    fn rate_limit_case_insensitive() {
        let m = matcher();
        assert!(ids(&m.match_line("Error: Rate Limit exceeded")).contains(&"rate_limit"));
        assert!(ids(&m.match_line("HTTP 429 too many requests")).contains(&"rate_limit"));
    }

    #[test]
    fn thinking_is_an_activity_hint() {
        let m = matcher();
        assert!(ids(&m.match_line("Working… (esc to interrupt)")).contains(&"thinking"));
    }

    #[test]
    fn turn_done_matches_idle_composer() {
        let m = matcher();
        assert!(ids(&m.match_line("> ")).contains(&"turn_done"));
        assert!(ids(&m.match_line("▌")).contains(&"turn_done"));
    }

    #[test]
    fn approval_prompt_requires_affordance() {
        let m = matcher();
        // Phrasing + affordance → fires.
        let hit = m.match_line("Allow command `rm -rf build`? ❯ Yes");
        assert!(ids(&hit).contains(&"approval_prompt"));
        // Phrasing only, no affordance → quiet.
        let no = m.match_line("I will run command to allow the build to proceed.");
        assert!(!ids(&no).contains(&"approval_prompt"));
    }

    #[test]
    fn plain_assistant_text_is_quiet() {
        let m = matcher();
        let hits = m.match_line("Here is the summary of what I changed.");
        let fired = ids(&hits);
        assert!(!fired.contains(&"rate_limit"));
        assert!(!fired.contains(&"approval_prompt"));
        assert!(!fired.contains(&"turn_done"));
    }
}
