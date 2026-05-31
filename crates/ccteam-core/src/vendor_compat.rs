//! Vendor-seam forward-compatibility helpers.
//!
//! ccteam reads vendor-owned outputs (`claude --bg` state.json, Codex
//! JSONL/app-server notifications). Unknown enum-like values must
//! degrade gracefully at the call site while warning once so operators
//! can spot vendor drift without log floods.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

/// Warn exactly once (per process) about an unrecognised vendor token.
///
/// `seam` identifies the parsing seam; `token` is the unknown string;
/// `detail` describes the fallback policy chosen by the caller.
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
        let first = warn_unknown_vendor_token("vc_core_once", "future_value", "did X");
        let second = warn_unknown_vendor_token("vc_core_once", "future_value", "did X");
        assert!(first, "first sighting of a token must warn");
        assert!(!second, "repeat sighting must be deduplicated");
    }

    #[test]
    fn same_token_distinct_seams_warn_independently() {
        assert!(warn_unknown_vendor_token(
            "vc_core_seam_a",
            "shared_tok",
            ""
        ));
        assert!(warn_unknown_vendor_token(
            "vc_core_seam_b",
            "shared_tok",
            ""
        ));
    }
}
