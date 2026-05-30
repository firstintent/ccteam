//! Layer-2 (TUI-render) pattern registry.
//!
//! ccteam's "no business-side grep" red line is preserved: the
//! orchestrator never scrapes pane text directly. Instead the daemon-
//! side `subscribe` translator runs a small, vetted set of regexes
//! against each completed output line and emits typed
//! [`crate::MuxEvent::PatternMatched`] events. This module owns that
//! vetted set + the matcher engine.
//!
//! These are **lossy Layer-2 signals** (W4 priority P2): when a richer
//! Layer-4 source exists (Claude Code hook subprocess / Codex
//! JSON-RPC) the W4 [`crate::MuxEvent`] merger prefers it and treats
//! the regex hit as a fallback. The patterns here are the "high
//! reliability" tier from the W2b research (§15.2) — chosen so that a
//! false positive is cheap (a spurious event the merger can suppress)
//! and a false negative is recoverable (the P1 source still fires).
//!
//! See `docs/versions/v0-8-rmux/w4-enriched-event-merger.md` for how
//! these `regex_id`s map onto the per-event-kind dispatch table.

use regex::Regex;

pub mod claude;
pub mod codex;

pub use claude::CLAUDE_BASE_PATTERNS;
pub use codex::CODEX_BASE_PATTERNS;

/// One entry in a vendor's base pattern table.
///
/// `id` is the stable `regex_id` surfaced on
/// [`crate::MuxEvent::PatternMatched`]; `regex` is the uncompiled
/// pattern source (validated by the per-table compile test so a typo
/// fails at test time, not at runtime).
#[derive(Debug, Clone, Copy)]
pub struct PatternEntry {
    pub id: &'static str,
    pub regex: &'static str,
}

/// Vendor selector for [`PatternMatcher::base`] / the backend
/// `register_base_patterns` convenience.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternVendor {
    Claude,
    // Codex base patterns are the §6.4 mode-3b L2 safety-net tier
    // (`w3b-codex-event-catalog.md`): a thin set of TUI regexes that
    // serve as lossy fallbacks for signals whose canonical source is
    // the Codex JSON-RPC channel. See `codex::CODEX_BASE_PATTERNS`.
    Codex,
}

/// Return the static base pattern table for a vendor.
///
/// Codex's table is deliberately thin (its semantic catalog is driven
/// by JSON-RPC, not TUI-render regexes — see `w3b`); the entries it
/// does carry are L2 fallbacks for when the typed channel is
/// unavailable.
pub fn base_patterns(vendor: PatternVendor) -> &'static [PatternEntry] {
    match vendor {
        PatternVendor::Claude => CLAUDE_BASE_PATTERNS,
        PatternVendor::Codex => CODEX_BASE_PATTERNS,
    }
}

/// A compiled set of `(regex_id, Regex)` pairs that a subscribe-side
/// translator consults per completed output line.
///
/// Cheap to clone via `Arc` at the backend layer; the matcher itself
/// holds owned compiled regexes. Insertion order is preserved so the
/// `regex_id`s fire in a deterministic sequence for a line that hits
/// multiple patterns.
#[derive(Debug, Clone, Default)]
pub struct PatternMatcher {
    patterns: Vec<(String, Regex)>,
}

impl PatternMatcher {
    /// Empty matcher — patterns added via [`Self::register`].
    pub fn new() -> Self {
        Self {
            patterns: Vec::new(),
        }
    }

    /// Build a matcher pre-loaded with a vendor's base patterns.
    ///
    /// Panics only on an internal table typo, which the per-table
    /// compile test (`claude::tests`) catches at test time — so this
    /// never panics for a shipped binary.
    pub fn base(vendor: PatternVendor) -> Self {
        let mut m = Self::new();
        for entry in base_patterns(vendor) {
            // unwrap: the compile test guarantees every base regex is
            // valid; a panic here means the test gate was skipped.
            m.register(entry.id.to_string(), entry.regex)
                .expect("base pattern regex must compile (guarded by compile test)");
        }
        m
    }

    /// Compile `regex` and store it under `regex_id`. Idempotent —
    /// re-registering the same `regex_id` replaces the prior pattern
    /// (mirrors the [`crate::MuxBackend::register_pattern`] contract).
    pub fn register(&mut self, regex_id: String, regex: &str) -> Result<(), regex::Error> {
        let compiled = Regex::new(regex)?;
        if let Some(slot) = self.patterns.iter_mut().find(|(id, _)| id == &regex_id) {
            slot.1 = compiled;
        } else {
            self.patterns.push((regex_id, compiled));
        }
        Ok(())
    }

    /// Run every registered pattern against `line`.
    ///
    /// Returns `(regex_id, captured)` for each hit, in registration
    /// order. `captured` is the first capture group when the pattern
    /// has one, else the whole match. A pattern that does not match
    /// contributes nothing.
    pub fn match_line(&self, line: &str) -> Vec<(String, String)> {
        let mut hits = Vec::new();
        for (id, re) in &self.patterns {
            if let Some(caps) = re.captures(line) {
                // Prefer the first explicit capture group; fall back to
                // the whole match (group 0 always exists when captures
                // is Some).
                let captured = caps
                    .get(1)
                    .or_else(|| caps.get(0))
                    .map(|m| m.as_str().to_string())
                    .unwrap_or_default();
                hits.push((id.clone(), captured));
            }
        }
        hits
    }

    /// Number of registered patterns. Test/observability helper.
    pub fn len(&self) -> usize {
        self.patterns.len()
    }

    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_matcher_matches_nothing() {
        let m = PatternMatcher::new();
        assert!(m.is_empty());
        assert!(m.match_line("● Read(/foo)").is_empty());
    }

    #[test]
    fn register_is_idempotent_on_id() {
        let mut m = PatternMatcher::new();
        m.register("k".to_string(), r"foo").unwrap();
        m.register("k".to_string(), r"bar").unwrap();
        assert_eq!(m.len(), 1);
        assert!(m.match_line("foo").is_empty());
        assert_eq!(m.match_line("bar").len(), 1);
    }

    #[test]
    fn register_rejects_bad_regex() {
        let mut m = PatternMatcher::new();
        assert!(m.register("k".to_string(), r"(unclosed").is_err());
    }

    #[test]
    fn base_claude_matcher_loads_all_entries() {
        let m = PatternMatcher::base(PatternVendor::Claude);
        assert_eq!(m.len(), CLAUDE_BASE_PATTERNS.len());
    }

    #[test]
    fn base_codex_matcher_loads_all_entries() {
        let m = PatternMatcher::base(PatternVendor::Codex);
        assert_eq!(m.len(), CODEX_BASE_PATTERNS.len());
        assert!(!m.is_empty());
    }
}
