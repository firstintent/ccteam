//! V0.6.0 Wave 1 — dual-vendor pricing tables + `estimate_cost`.
//!
//! Two TOML tables are bundled via `include_str!` and parsed once per
//! `Vendor` through a per-vendor `OnceLock<PricingTable>`. Unknown
//! model ids fall back to the table's `fallback_model` with a WARN-once
//! per `(vendor, model)` so a model-id drift still returns a sensible
//! cost rather than `None`.
//!
//! ## Cross-vendor field semantics
//!
//! `UnifiedTokenUsage` is the union of the two vendors' usage shapes:
//!
//! | Field | Claude bucket | Codex bucket |
//! |---|---|---|
//! | `input_tokens` | `input_tokens` | `input_tokens` |
//! | `cached_input_tokens` | `cache_read_input_tokens` | `cached_input_tokens` |
//! | `output_tokens` | `output_tokens` | `output_tokens` |
//! | `cache_creation_input_tokens` | `cache_creation_input_tokens` (1h ephemeral cache *write*) | n/a |
//! | `reasoning_output_tokens` | n/a | hidden CoT reasoning tokens (o-series) |
//!
//! `serde` aliases on the renamed Claude field
//! (`cache_read_input_tokens` → `cached_input_tokens`) keep
//! deserialisation of raw Claude transcript JSONL working without a
//! caller-side shim.
//!
//! ## Red lines
//!
//! - **Bundled**, never fetched at runtime. `ccteam doctor
//!   --check-pricing-version` is the operator-facing staleness warn.
//! - **Lossy model match** — Claude Code's `respawnFlags --model` value
//!   sometimes carries the `[1m]` 1M-context suffix; the matcher strips
//!   it before vendor lookup.

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use tracing::warn;

/// Anthropic Claude price table. Verified against
/// `platform.claude.com/docs/en/about-claude/pricing` on the
/// `schema_version` date inside the file.
const ANTHROPIC_TOML: &str = include_str!("../pricing/anthropic.toml");

/// OpenAI Codex price table (o-series + gpt-4o family). Verified
/// against `openai.com/api/pricing` on the `schema_version` date.
const OPENAI_TOML: &str = include_str!("../pricing/openai.toml");

/// Vendor discriminator. Defined here (not in `ccteam-core::harness`)
/// so this crate has zero `ccteam-core` deps; `ccteam-core` re-exports
/// `ccteam_cost::Vendor` from its `lib.rs` for downstream callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Vendor {
    Claude,
    Codex,
}

/// Dual-vendor token usage shape — unified across Claude + Codex.
///
/// Field-level `serde(alias = ...)` keeps Claude transcript JSONL
/// (which writes `cache_read_input_tokens`) deserialisable into this
/// shape without renaming on the caller side.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct UnifiedTokenUsage {
    #[serde(default)]
    pub input_tokens: u64,
    /// Both vendors: cached prompt tokens (Claude's
    /// `cache_read_input_tokens`, Codex's `cached_input_tokens`).
    #[serde(default, alias = "cache_read_input_tokens")]
    pub cached_input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    /// Anthropic-only: 1h ephemeral cache *write* tokens. `None` for
    /// Codex (Codex prompt cache is read-only — no creation SKU).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u64>,
    /// Codex-only: hidden chain-of-thought tokens billed at the
    /// output rate (o-series). `None` for Claude.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_output_tokens: Option<u64>,
}

impl UnifiedTokenUsage {
    /// Sum every bucket — used as a quick "any usage at all?" check.
    pub fn total(&self) -> u64 {
        self.input_tokens
            + self.cached_input_tokens
            + self.output_tokens
            + self.cache_creation_input_tokens.unwrap_or(0)
            + self.reasoning_output_tokens.unwrap_or(0)
    }
}

/// Per-model rate sheet. All fields are dollars per **1M** tokens.
///
/// Vendor asymmetry is encoded in the `Option<>` fields:
/// - Anthropic models populate `cache_creation_per_1m` (1h ephemeral
///   cache write SKU). Codex models leave it `None`.
/// - Codex o-series populate `reasoning_per_1m` (CoT hidden tokens,
///   usually `== output_per_1m`). gpt-4o family + Anthropic leave it
///   `None`.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct ModelPrices {
    pub input_per_1m: f64,
    pub cached_input_per_1m: f64,
    pub output_per_1m: f64,
    #[serde(default)]
    pub cache_creation_per_1m: Option<f64>,
    #[serde(default)]
    pub reasoning_per_1m: Option<f64>,
}

/// Parsed pricing TOML (one per vendor).
#[derive(Debug, Deserialize)]
struct PricingTable {
    schema_version: String,
    models: HashMap<String, ModelPrices>,
    fallback_model: String,
}

static ANTHROPIC_TABLE: OnceLock<PricingTable> = OnceLock::new();
static OPENAI_TABLE: OnceLock<PricingTable> = OnceLock::new();

fn table_for(vendor: Vendor) -> &'static PricingTable {
    match vendor {
        Vendor::Claude => ANTHROPIC_TABLE.get_or_init(|| {
            toml::from_str(ANTHROPIC_TOML)
                .expect("ccteam anthropic.toml embedded at compile time must parse")
        }),
        Vendor::Codex => OPENAI_TABLE.get_or_init(|| {
            toml::from_str(OPENAI_TOML)
                .expect("ccteam openai.toml embedded at compile time must parse")
        }),
    }
}

/// Compute the dollar cost of one usage block.
///
/// Unknown models fall back to the vendor table's `fallback_model` and
/// emit a single WARN per process per `(vendor, model)`. The matcher
/// is permissive: it strips the optional `[1m]` 1M-context suffix
/// Anthropic attaches to model strings so callers can pass
/// `claude-opus-4-7[1m]` directly.
pub fn estimate_cost(usage: &UnifiedTokenUsage, vendor: Vendor, model: &str) -> f64 {
    let prices = resolve(vendor, model);
    let from_input = usage.input_tokens as f64 * prices.input_per_1m;
    let from_cached = usage.cached_input_tokens as f64 * prices.cached_input_per_1m;
    let from_output = usage.output_tokens as f64 * prices.output_per_1m;
    let from_cache_create = match (
        usage.cache_creation_input_tokens,
        prices.cache_creation_per_1m,
    ) {
        (Some(toks), Some(rate)) => toks as f64 * rate,
        _ => 0.0,
    };
    let from_reasoning = match (usage.reasoning_output_tokens, prices.reasoning_per_1m) {
        (Some(toks), Some(rate)) => toks as f64 * rate,
        // Codex o-series may report reasoning tokens without an
        // explicit reasoning_per_1m entry — bill them at the output
        // rate (the documented vendor behaviour).
        (Some(toks), None) if matches!(vendor, Vendor::Codex) => toks as f64 * prices.output_per_1m,
        _ => 0.0,
    };
    (from_input + from_cached + from_output + from_cache_create + from_reasoning) / 1_000_000.0
}

/// Embedded price table's schema version (the more recently dated of
/// the two vendor tables — typically `YYYY-MM-DD`). Used by
/// `ccteam doctor --check-pricing-version` to surface a staleness WARN
/// when the in-binary tables outrun their useful life.
pub fn pricing_schema_version() -> &'static str {
    let a = table_for(Vendor::Claude).schema_version.as_str();
    let o = table_for(Vendor::Codex).schema_version.as_str();
    // Lexicographic compare works for YYYY-MM-DD; pick the newer.
    if o > a {
        o
    } else {
        a
    }
}

/// Per-vendor `schema_version`. Lets `doctor` print both rows when
/// emitting staleness diagnostics.
pub fn pricing_schema_version_for(vendor: Vendor) -> &'static str {
    table_for(vendor).schema_version.as_str()
}

/// Look up `model` in the vendor table. Strips the `[1m]` suffix and
/// tries the raw id; falls back to the table's `fallback_model`
/// (WARN-once) on miss.
fn resolve(vendor: Vendor, model: &str) -> ModelPrices {
    let normalized = normalize_model_id(model);
    let tbl = table_for(vendor);
    if let Some(p) = tbl.models.get(&normalized) {
        return *p;
    }
    warn_unknown_model_once(vendor, &normalized);
    *tbl.models
        .get(&tbl.fallback_model)
        .expect("pricing TOML fallback_model row must exist")
}

/// `claude-opus-4-7[1m]` → `claude-opus-4-7`. Other suffixes pass
/// through unchanged.
fn normalize_model_id(model: &str) -> String {
    if let Some(idx) = model.find('[') {
        return model[..idx].to_string();
    }
    model.to_string()
}

fn warn_unknown_model_once(vendor: Vendor, model: &str) {
    use std::sync::Mutex;
    static SEEN: OnceLock<Mutex<std::collections::HashSet<(Vendor, String)>>> = OnceLock::new();
    let lock = SEEN.get_or_init(|| Mutex::new(std::collections::HashSet::new()));
    let mut set = lock.lock().expect("pricing warn-once mutex poisoned");
    if set.insert((vendor, model.to_string())) {
        warn!(
            vendor = ?vendor,
            model = %model,
            "unknown model id; falling back to vendor pricing table's fallback_model. Bump ccteam pricing table.",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- migrated from ccteam-core::pricing (V0.5.0 F92) -----------------

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
        let cost = estimate_cost(
            &UnifiedTokenUsage::default(),
            Vendor::Claude,
            "claude-sonnet-4-6",
        );
        assert!(cost.abs() < 1e-12);
    }

    #[test]
    fn normalize_strips_1m_suffix() {
        assert_eq!(normalize_model_id("claude-opus-4-7[1m]"), "claude-opus-4-7");
        assert_eq!(normalize_model_id("claude-sonnet-4-6"), "claude-sonnet-4-6");
    }

    #[test]
    fn estimate_cost_unknown_claude_model_falls_back() {
        let cost = estimate_cost(
            &UnifiedTokenUsage {
                input_tokens: 1_000_000,
                ..Default::default()
            },
            Vendor::Claude,
            "claude-future-99",
        );
        assert!(cost > 0.0);
    }

    // -- new in V0.6.0 Wave 1 --------------------------------------------

    #[test]
    fn estimate_cost_codex_o3_input_matches_table() {
        let one_m_input = UnifiedTokenUsage {
            input_tokens: 1_000_000,
            ..Default::default()
        };
        // o3 input = $2 / 1M.
        let cost = estimate_cost(&one_m_input, Vendor::Codex, "o3");
        assert!((cost - 2.0).abs() < 0.01, "o3 input != $2 / 1M (got {cost})");
    }

    #[test]
    fn estimate_cost_codex_reasoning_billed_as_output() {
        // o3 reasoning = $8 / 1M (== output rate).
        let usage = UnifiedTokenUsage {
            reasoning_output_tokens: Some(1_000_000),
            ..Default::default()
        };
        let cost = estimate_cost(&usage, Vendor::Codex, "o3");
        assert!(
            (cost - 8.0).abs() < 0.01,
            "o3 reasoning != $8 / 1M (got {cost})",
        );
    }

    #[test]
    fn estimate_cost_codex_unknown_model_falls_back() {
        let usage = UnifiedTokenUsage {
            input_tokens: 1_000_000,
            ..Default::default()
        };
        let cost = estimate_cost(&usage, Vendor::Codex, "o9-imaginary");
        assert!(cost > 0.0, "fallback to o3 must yield positive cost");
    }

    #[test]
    fn dual_vendor_pricing_isolated() {
        // Same usage block, different vendor → different cost.
        let usage = UnifiedTokenUsage {
            input_tokens: 1_000_000,
            ..Default::default()
        };
        let claude = estimate_cost(&usage, Vendor::Claude, "claude-sonnet-4-6"); // $3
        let codex = estimate_cost(&usage, Vendor::Codex, "o3"); // $2
        assert!((claude - 3.0).abs() < 0.01);
        assert!((codex - 2.0).abs() < 0.01);
        assert!(
            (claude - codex).abs() > 0.5,
            "vendors must price independently (claude={claude}, codex={codex})",
        );
    }

    #[test]
    fn anthropic_cache_creation_charged_separately() {
        // 1M cache_creation @ sonnet-4-6 = $3.75 / 1M.
        let usage = UnifiedTokenUsage {
            cache_creation_input_tokens: Some(1_000_000),
            ..Default::default()
        };
        let cost = estimate_cost(&usage, Vendor::Claude, "claude-sonnet-4-6");
        assert!(
            (cost - 3.75).abs() < 0.01,
            "sonnet-4-6 cache_creation != $3.75 / 1M (got {cost})",
        );
    }

    // -- UnifiedTokenUsage serde round-trip (≥2 per briefing) ------------

    #[test]
    fn unified_token_usage_round_trips_through_serde_json() {
        let original = UnifiedTokenUsage {
            input_tokens: 100,
            cached_input_tokens: 50,
            output_tokens: 200,
            cache_creation_input_tokens: Some(25),
            reasoning_output_tokens: Some(75),
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let back: UnifiedTokenUsage = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.input_tokens, original.input_tokens);
        assert_eq!(back.cached_input_tokens, original.cached_input_tokens);
        assert_eq!(back.output_tokens, original.output_tokens);
        assert_eq!(
            back.cache_creation_input_tokens,
            original.cache_creation_input_tokens,
        );
        assert_eq!(
            back.reasoning_output_tokens,
            original.reasoning_output_tokens,
        );
        assert_eq!(back.total(), original.total());
    }

    #[test]
    fn unified_token_usage_accepts_claude_cache_read_alias() {
        // Claude transcript JSONL writes `cache_read_input_tokens`,
        // not `cached_input_tokens`. The alias must absorb it so
        // `transcript_scanner::sum_usage` deserialises straight.
        let claude_shape = serde_json::json!({
            "input_tokens": 10,
            "cache_read_input_tokens": 20,
            "cache_creation_input_tokens": 5,
            "output_tokens": 30,
        });
        let usage: UnifiedTokenUsage =
            serde_json::from_value(claude_shape).expect("claude alias round-trip");
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.cached_input_tokens, 20);
        assert_eq!(usage.cache_creation_input_tokens, Some(5));
        assert_eq!(usage.output_tokens, 30);
        assert_eq!(usage.reasoning_output_tokens, None);
    }

    #[test]
    fn unified_token_usage_total_sums_every_bucket() {
        let u = UnifiedTokenUsage {
            input_tokens: 1,
            cached_input_tokens: 2,
            output_tokens: 4,
            cache_creation_input_tokens: Some(8),
            reasoning_output_tokens: Some(16),
        };
        assert_eq!(u.total(), 1 + 2 + 4 + 8 + 16);
    }
}
