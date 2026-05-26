//! W2b — integration coverage for the Claude base pattern matcher.
//!
//! Feeds realistic Claude TUI line samples through the public
//! `PatternMatcher` and asserts the right `regex_id`s fire with the
//! right captured group. Complements the in-crate unit tests in
//! `patterns/claude.rs` by exercising the matcher through the crate's
//! public API exactly as a backend `subscribe` translator would.

use ccteam_mux::patterns::{PatternMatcher, PatternVendor};

fn matcher() -> PatternMatcher {
    PatternMatcher::base(PatternVendor::Claude)
}

fn fired(line: &str) -> Vec<(String, String)> {
    matcher().match_line(line)
}

fn ids(hits: &[(String, String)]) -> Vec<&str> {
    hits.iter().map(|(id, _)| id.as_str()).collect()
}

fn captured(hits: &[(String, String)], id: &str) -> Option<String> {
    hits.iter()
        .find(|(rid, _)| rid == id)
        .map(|(_, c)| c.clone())
}

#[test]
fn tool_call_started_captures_various_tool_names() {
    for (line, tool) in [
        ("● Read(/foo/bar.rs)", "Read"),
        ("● Bash(cargo test)", "Bash"),
        ("● Edit(src/lib.rs)", "Edit"),
        ("● Grep(pattern)", "Grep"),
    ] {
        let hits = fired(line);
        assert!(
            ids(&hits).contains(&"tool_call_started"),
            "expected tool_call_started for `{line}`"
        );
        assert_eq!(captured(&hits, "tool_call_started").as_deref(), Some(tool));
    }
}

#[test]
fn tool_call_completed_on_result_continuation() {
    for line in ["  ⎿  Read 42 lines", "⎿ Done", "    ⎿  (no output)"] {
        assert!(
            ids(&fired(line)).contains(&"tool_call_completed"),
            "expected tool_call_completed for `{line}`"
        );
    }
}

#[test]
fn permission_prompt_needs_question_and_affordance() {
    assert!(ids(&fired("Do you want to proceed? ❯ 1. Yes")).contains(&"permission_prompt"));
    assert!(ids(&fired("Do you want to allow this command? [y]/n")).contains(&"permission_prompt"));
    // Question text without an affordance must not fire.
    assert!(
        !ids(&fired("Do you want to allow that, generally speaking?"))
            .contains(&"permission_prompt")
    );
}

#[test]
fn rate_limit_and_context_overflow() {
    assert!(ids(&fired("Error: rate limit reached, retry in 30s")).contains(&"rate_limit"));
    assert!(ids(&fired("HTTP 429 Too Many Requests")).contains(&"rate_limit"));
    assert!(ids(&fired("Context window is getting low")).contains(&"context_overflow"));
    assert!(ids(&fired("Auto-compact triggered")).contains(&"context_overflow"));
}

#[test]
fn token_usage_capture() {
    assert_eq!(
        captured(&fired("45000 tokens used"), "token_usage").as_deref(),
        Some("45000")
    );
    assert_eq!(
        captured(&fired("Context: 1.2k tokens"), "token_usage").as_deref(),
        Some("1.2k")
    );
}

#[test]
fn user_prompt_and_session_reset_and_turn_done() {
    assert_eq!(
        captured(&fired("> fix the failing test"), "user_prompt_submit").as_deref(),
        Some("fix the failing test")
    );
    assert!(ids(&fired("✻ Welcome to Claude Code!")).contains(&"session_reset"));
    // turn_done heuristic: empty prompt / prompt-box bottom border.
    assert!(ids(&fired("> ")).contains(&"turn_done"));
    assert!(ids(&fired("╰──────────╯")).contains(&"turn_done"));
}

#[test]
fn registered_custom_pattern_fires() {
    // The same matcher engine the backends use for register_pattern.
    let mut m = PatternMatcher::base(PatternVendor::Claude);
    m.register("custom_done".to_string(), r"BUILD SUCCESSFUL")
        .unwrap();
    let hits = m.match_line("BUILD SUCCESSFUL in 3s");
    assert!(hits.iter().any(|(id, _)| id == "custom_done"));
}

#[test]
fn plain_prose_is_mostly_quiet() {
    // A normal assistant sentence must not trip the high-confidence
    // structural patterns.
    let hits = fired("I have updated the function and added a test for it.");
    let f = ids(&hits);
    assert!(!f.contains(&"tool_call_started"));
    assert!(!f.contains(&"tool_call_completed"));
    assert!(!f.contains(&"permission_prompt"));
    assert!(!f.contains(&"session_reset"));
    assert!(!f.contains(&"rate_limit"));
}
