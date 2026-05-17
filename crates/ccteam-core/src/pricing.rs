//! V0.5.0 F92 — Anthropic price table + token → dollar estimator.
//!
//! The pricing data is bundled in [`PRICING_JSON`] via `include_str!`
//! and parsed once at first access through a `OnceLock`. The shape:
//!
//! ```json
//! {
//!   "schema_version": "YYYY-MM-DD",
//!   "models": {
//!     "claude-sonnet-4-6": {
//!       "input_per_1m": 3.0,
//!       "cache_creation_per_1m": 3.75,
//!       "cache_read_per_1m": 0.30,
//!       "output_per_1m": 15.0
//!     }
//!   },
//!   "fallback_model": "claude-sonnet-4-6"
//! }
//! ```
//!
//! ## Red lines
//!
//! - **Bundled**, never fetched at runtime. `ccteam doctor
//!   --check-pricing-version` is the operator-facing staleness warn.
//! - **Lossy model match** — Claude Code's `respawnFlags --model` value
//!   sometimes carries the `[1m]` 1M-context suffix; the matcher strips
//!   it. Unknown models fall back to `fallback_model` (sonnet-4-6) so
//!   F92 still returns a sensible cost rather than `None` on a model id
//!   drift. The fallback is logged WARN-once per unknown model.

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::Deserialize;
use tracing::warn;

/// Embedded price table — verified against
/// `platform.claude.com/docs/en/about-claude/pricing` on the
/// schema_version date inside this file. See [`pricing_schema_version`].
const PRICING_JSON: &str = include_str!("pricing.json");

/// Parsed price table. Wrapped in a `OnceLock` so we parse the JSON
/// once at first access and reuse for every subsequent
/// [`estimate_cost`] call.
static TABLE: OnceLock<PricingTable> = OnceLock::new();

fn table() -> &'static PricingTable {
    TABLE.get_or_init(|| {
        serde_json::from_str(PRICING_JSON)
            .expect("ccteam pricing.json embedded at compile time must parse")
    })
}

/// Parsed `pricing.json`.
#[derive(Debug, Deserialize)]
struct PricingTable {
    schema_version: String,
    models: HashMap<String, ModelPrices>,
    fallback_model: String,
}

/// Per-model rate sheet. All fields are dollars per **1M** tokens.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct ModelPrices {
    pub input_per_1m: f64,
    pub cache_creation_per_1m: f64,
    pub cache_read_per_1m: f64,
    pub output_per_1m: f64,
}

/// Token counts for one Claude turn — the four buckets that appear in
/// every `message.usage` block written by Claude Code into the
/// transcript JSONL. Field names match the JSONL keys verbatim so a
/// caller can `serde_json::from_value(usage_obj)` straight into it.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
}

impl Usage {
    /// Sum the four buckets — used as a quick "any usage at all?" check.
    pub fn total(&self) -> u64 {
        self.input_tokens
            + self.cache_creation_input_tokens
            + self.cache_read_input_tokens
            + self.output_tokens
    }
}

/// Compute the dollar cost of one usage block for `model`.
///
/// Unknown models fall back to the table's `fallback_model` and emit a
/// single WARN per process per unknown id. The matcher is permissive:
/// it strips the optional `[1m]` 1M-context suffix Claude Code attaches
/// to model strings so callers can pass `claude-opus-4-7[1m]` directly.
pub fn estimate_cost(model: &str, usage: &Usage) -> f64 {
    let prices = resolve(model);
    let from_input = usage.input_tokens as f64 * prices.input_per_1m;
    let from_cache_create = usage.cache_creation_input_tokens as f64 * prices.cache_creation_per_1m;
    let from_cache_read = usage.cache_read_input_tokens as f64 * prices.cache_read_per_1m;
    let from_output = usage.output_tokens as f64 * prices.output_per_1m;
    (from_input + from_cache_create + from_cache_read + from_output) / 1_000_000.0
}

/// Embedded price table's schema version (typically `YYYY-MM-DD`). Used
/// by `ccteam doctor --check-pricing-version` to surface a staleness
/// WARN when the in-binary table outruns its useful life.
pub fn pricing_schema_version() -> &'static str {
    table().schema_version.as_str()
}

/// Look up `model` in the table. Strips the `[1m]` suffix and tries the
/// raw id; falls back to the table's `fallback_model` (with a WARN) on
/// miss.
fn resolve(model: &str) -> ModelPrices {
    let normalized = normalize_model_id(model);
    let tbl = table();
    if let Some(p) = tbl.models.get(&normalized) {
        return *p;
    }
    warn_unknown_model_once(&normalized);
    // The fallback_model entry is verified to exist at parse time —
    // PricingTable::models is a HashMap, but the JSON authoring guarantees
    // the fallback row is present. Failing here would be a misconfigured
    // pricing.json, not a runtime error worth bubbling up.
    *tbl.models
        .get(&tbl.fallback_model)
        .expect("pricing.json fallback_model row must exist")
}

/// `claude-opus-4-7[1m]` → `claude-opus-4-7`. Other suffixes pass
/// through.
fn normalize_model_id(model: &str) -> String {
    if let Some(idx) = model.find('[') {
        return model[..idx].to_string();
    }
    model.to_string()
}

fn warn_unknown_model_once(model: &str) {
    use std::sync::Mutex;
    static SEEN: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    let lock = SEEN.get_or_init(|| Mutex::new(std::collections::HashSet::new()));
    let mut set = lock.lock().expect("pricing warn-once mutex poisoned");
    if set.insert(model.to_string()) {
        warn!(
            model = %model,
            "unknown model id; falling back to pricing.json::fallback_model. Bump ccteam pricing table."
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_parses_and_exposes_schema_version() {
        let v = pricing_schema_version();
        assert!(
            v.len() == 10 && v.chars().nth(4) == Some('-') && v.chars().nth(7) == Some('-'),
            "schema_version must look like YYYY-MM-DD, got {v:?}",
        );
    }

    #[test]
    fn estimate_cost_zero_usage_is_zero() {
        let cost = estimate_cost("claude-sonnet-4-6", &Usage::default());
        assert!(cost.abs() < 1e-12);
    }

    #[test]
    fn normalize_strips_1m_suffix() {
        assert_eq!(normalize_model_id("claude-opus-4-7[1m]"), "claude-opus-4-7");
        assert_eq!(normalize_model_id("claude-sonnet-4-6"), "claude-sonnet-4-6");
    }

    #[test]
    fn estimate_cost_unknown_model_falls_back() {
        // Unknown ids fall back to fallback_model; the assertion is just
        // that the call succeeds and returns a positive cost for
        // positive usage. The WARN-once is exercised at runtime.
        let cost = estimate_cost(
            "claude-future-99",
            &Usage {
                input_tokens: 1_000_000,
                ..Default::default()
            },
        );
        assert!(cost > 0.0);
    }
}
