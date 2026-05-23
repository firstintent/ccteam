//! V0.6.3 F144 — vendor-seam forward-compatibility helpers.
//!
//! ## Why this module exists
//!
//! ccteam is a meta-harness: it reads what Claude Code and Codex emit
//! (background-job `state.json`, `codex exec --json` JSONL, `codex
//! app-server` JSON-RPC notifications). Those CLIs are third-party and
//! ship on their own cadence — Anthropic / OpenAI add a JSON field or a
//! new status / event-kind value whenever they like. ccteam **cannot**
//! wipe-and-reinit that data (the "no historical migration" red line
//! governs ccteam's *own* state; vendor-owned files are a different
//! axis). So the vendor-reading seam must **degrade gracefully** instead
//! of panicking or misbehaving.
//!
//! ## The contract
//!
//! - Unknown / extra JSON fields on vendor output are ignored. Every
//!   vendor-output struct in `ccteam-core` already deserialises through
//!   `serde_json::Value` plucking (or `#[serde(default)]` structs with
//!   no `deny_unknown_fields`), so an added field is a no-op by
//!   construction. No code change is needed for that half.
//! - Unknown *enum-like* values (a `state.json::state` string we don't
//!   recognise, a `codex exec` event `type` we don't translate, a
//!   `codex app-server` notification `method` we don't propagate) are
//!   handled by the call site — but **must warn once** so an operator
//!   can spot a vendor drift in the logs without the warning flooding
//!   on every poll tick.
//!
//! This module owns only that warn-once dedup. The degradation policy
//! itself (unknown job status = non-terminal, unknown event = skip)
//! lives at each call site because it is call-site-specific.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

/// Process-wide dedup set for vendor-drift warnings. Keyed by
/// `"<seam>:<token>"` so e.g. an unknown Claude job state and an unknown
/// Codex event with the same literal token still warn independently.
static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

/// Warn exactly once (per process) about an unrecognised vendor token.
///
/// `seam` identifies the parsing seam (e.g. `"claude_job_state"`,
/// `"codex_exec_event"`, `"codex_app_server_notification"`); `token` is
/// the actual unknown string the vendor sent. `detail` is appended to
/// the log line so the operator knows what ccteam did instead (e.g.
/// "treating as non-terminal; will keep probing").
///
/// Subsequent calls with the same `(seam, token)` pair are silent —
/// vendor drift would otherwise log on every daemon poll tick.
///
/// Returns `true` when this call actually emitted a warning (the dedup
/// set was extended), `false` when it was deduplicated. The return value
/// lets tests assert warn-once behaviour deterministically without a
/// shared global counter that would race under parallel test execution.
pub fn warn_unknown_vendor_token(seam: &str, token: &str, detail: &str) -> bool {
    let key = format!("{seam}:{token}");
    let lock = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
    let mut set = match lock.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    if set.insert(key) {
        tracing::warn!(
            seam = %seam,
            token = %token,
            "vendor forward-compat: unrecognised value from sub-harness output; {detail}",
        );
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warns_once_per_token_then_dedups() {
        // Distinct token literal so this test doesn't collide with the
        // dedup state of other tests in the same process.
        let first = warn_unknown_vendor_token("vc_test_once", "future_value", "did X");
        let second = warn_unknown_vendor_token("vc_test_once", "future_value", "did X");
        let third = warn_unknown_vendor_token("vc_test_once", "future_value", "did X");
        assert!(first, "first sighting of a token must warn");
        assert!(!second, "repeat sighting must be deduplicated");
        assert!(!third, "repeat sighting must be deduplicated");
    }

    #[test]
    fn distinct_tokens_warn_independently() {
        assert!(warn_unknown_vendor_token("vc_test_distinct", "tok_one", ""));
        assert!(warn_unknown_vendor_token("vc_test_distinct", "tok_two", ""));
    }

    #[test]
    fn same_token_distinct_seams_warn_independently() {
        assert!(warn_unknown_vendor_token(
            "vc_test_seam_a",
            "shared_tok",
            ""
        ));
        assert!(
            warn_unknown_vendor_token("vc_test_seam_b", "shared_tok", ""),
            "seam prefix keeps tokens namespaced"
        );
    }
}
