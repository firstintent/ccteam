//! Pull-mode context usage for ACP vendors that never push it.
//!
//! ## Why a pull mode exists at all
//!
//! ACP standardises the content stream but leaves token accounting in its
//! UNSTABLE area: of the eleven `session/update` variants only `usage_update`
//! — the one carrying `{used, size}` — is marked "not part of the spec yet,
//! may be removed or changed at any point", and `ModelInfo` has no
//! context-window field in any form. Vendors land wherever that leaves them:
//! OpenCode pushes `usage_update`, grok puts per-turn totals in `_meta` and
//! the window in its model catalog's `_meta`, and Kimi sends neither.
//!
//! Kimi does answer, though — through the other, *stable* half of the
//! protocol. It advertises `status` / `usage` in `available_commands_update`,
//! and those commands are handled inside its ACP layer without a model call:
//! measured on a live 0.26.0 binary, `/status` replies in 15–21 ms with
//! `- Context: 26,284 / 1,048,576 (2.5%)` and tracks real occupancy across
//! turns. So the number is on a contract surface (a command the vendor
//! publishes), not in its private session logs — which is the line that
//! decides whether reading it is fair game.
//!
//! ## What keeps this honest
//!
//! Parsing a human-readable line is the weak joint, so it is fenced three ways:
//!
//! 1. **Fail-closed.** Anything unexpected parses to `None` and the session
//!    reports "usage unknown" — the pre-existing state. A probe can never
//!    produce a wrong number, only no number.
//! 2. **Lowest priority.** Any push or derived channel outranks it, so the day
//!    kimi emits `usage_update` the probe stops being consulted and this
//!    module becomes deletable. It is temporary by construction.
//! 3. **Discovered, never assumed.** The command must appear in the vendor's
//!    own advertised catalog before we send it — if kimi drops `/status`, we
//!    stop asking instead of prompting blindly.

use crate::ContextUsage;

/// A vendor's pull-mode context surface: which advertised command to ask, and
/// how to read its answer.
#[derive(Clone, Copy)]
pub struct AcpContextProbe {
    /// Command name as it appears in `available_commands_update`. Sent as
    /// `/<command>` — the ACP way a client invokes an advertised command.
    pub command: &'static str,
    /// Parse the command's reply text. `None` for anything unrecognised.
    pub parse: fn(&str) -> Option<ContextUsage>,
}

impl std::fmt::Debug for AcpContextProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcpContextProbe")
            .field("command", &self.command)
            .finish_non_exhaustive()
    }
}

/// Kimi Code's `/status` (and `/usage`, same line) context report.
///
/// Format pinned against `kimi-code`'s `formatStatusReport`:
/// `- Context: <used> / <max> (<pct>%)`, thousands-separated, with `unknown`
/// standing in for a max the vendor has not resolved.
pub const KIMI_STATUS_PROBE: AcpContextProbe = AcpContextProbe {
    command: "status",
    parse: parse_kimi_status,
};

/// Read `- Context: 26,284 / 1,048,576 (2.5%)` out of a `/status` report.
///
/// Returns `None` unless BOTH numbers are present and the window is non-zero:
/// a used count with no window would be a different, weaker claim than the one
/// this probe exists to make, and guessing the window is what we refuse to do.
fn parse_kimi_status(text: &str) -> Option<ContextUsage> {
    let line = text.lines().find(|l| l.contains("Context:"))?;
    let (_, rest) = line.split_once("Context:")?;
    let (used_part, window_part) = rest.split_once('/')?;
    let used = parse_grouped_u64(used_part)?;
    // Trim the trailing ` (2.5%)` before reading the window.
    let window_part = window_part.split('(').next().unwrap_or(window_part);
    let window = parse_grouped_u64(window_part)?;
    if window == 0 {
        return None;
    }
    Some(ContextUsage::known(
        used,
        window,
        crate::ContextSource::Probed,
    ))
}

/// Parse a thousands-separated integer, ignoring surrounding whitespace.
/// Rejects a fragment carrying any other character (`unknown`, a percentage,
/// a stray unit) rather than salvaging digits out of it.
fn parse_grouped_u64(s: &str) -> Option<u64> {
    let t = s.trim();
    if t.is_empty() || !t.chars().all(|c| c.is_ascii_digit() || c == ',') {
        return None;
    }
    t.replace(',', "").parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ContextSource;

    /// Byte-for-byte the reply of a live `kimi acp` 0.26.0 binary, captured
    /// after two turns. If a kimi upgrade changes this rendering, this test is
    /// where it must be noticed — the probe is deliberately pinned to a format
    /// we verified, not to a guess.
    const LIVE_KIMI_STATUS: &str = "Session status:\n\
        - Model: kimi-code/k3\n\
        - Thinking: max\n\
        - Permission: manual\n\
        - Plan mode: off\n\
        - Context: 26,284 / 1,048,576 (2.5%)";

    #[test]
    fn parses_the_live_status_report() {
        let ctx = (KIMI_STATUS_PROBE.parse)(LIVE_KIMI_STATUS).expect("live format must parse");
        assert_eq!(ctx.used_tokens, Some(26_284));
        assert_eq!(ctx.window_tokens, 1_048_576);
        assert_eq!(ctx.source, ContextSource::Probed);
        // Kimi's window is 2^20, not a round million — `1.0M`, not `1M`.
        assert_eq!(ctx.render(), "26.3k / 1.0M (3%)");
    }

    #[test]
    fn parses_a_fresh_session_at_zero() {
        let ctx = (KIMI_STATUS_PROBE.parse)("- Context: 0 / 1,048,576 (0.0%)").unwrap();
        assert_eq!(ctx.used_tokens, Some(0));
        assert_eq!(ctx.window_tokens, 1_048_576);
    }

    /// Fail-closed: every malformed shape must yield "unknown", never a
    /// half-read number. The cost of a miss is the status quo; the cost of a
    /// wrong number is a user trusting it.
    #[test]
    fn refuses_anything_it_does_not_fully_understand() {
        for bad in [
            "",
            "Session status:\n- Model: kimi-code/k3", // no Context line
            "- Context: 26,284",                      // no window
            "- Context: 26,284 / unknown",            // unresolved window
            "- Context: 26,284 / 0 (0%)",             // zero window
            "- Context: about 26k / 1,048,576",       // non-numeric used
            "- Context: 26,284 / 1,048,576 tokens (2.5%)", // unexpected unit
            "Unknown ACP command: /status. Use /help.", // vendor dropped it
        ] {
            assert!(
                (KIMI_STATUS_PROBE.parse)(bad).is_none(),
                "must refuse to parse: {bad:?}"
            );
        }
    }
}
