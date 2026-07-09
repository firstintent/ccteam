//! V0.6.0 Wave 1 — dual-vendor pricing tables + `estimate_cost`.
//!
//! Two TOML tables are bundled via `include_str!` and parsed once per
//! `Vendor` through a per-vendor `OnceLock<PricingTable>`. A model id
//! that is **not** a key in the vendor table prices to `None` (the
//! caller renders "—" / excludes it) with a WARN-once per
//! `(vendor, model)` so the unknown model surfaces in the logs.
//!
//! ## Determinism (no silent fallback)
//!
//! There is **no** `fallback_model`: a cost is returned only when the
//! supplied model is a real, table-matched id. An unknown / absent model
//! is exposed (returns `None`) rather than being billed at some other
//! model's rate behind a wrong-but-plausible number. The deterministic
//! source for the model is the transcript's per-message `message.model`
//! (canonical — e.g. `claude-opus-4-8`), not the `/model` alias
//! (`default` / `opus[1m]`).
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
//! - **`[1m]` suffix tolerance** — a model id may carry the `[1m]`
//!   1M-context suffix (e.g. `claude-opus-4-8[1m]`); the matcher strips
//!   it before vendor lookup. (Note: cost is sourced from the canonical
//!   per-message `message.model`, which is already the bare id — the
//!   strip covers the rare caller that still passes an aliased id.)

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

/// xAI Grok price table. Empty models map is honest when no public
/// USD rates are embedded — `estimate_cost` returns `None` → UI "—".
const XAI_TOML: &str = include_str!("../pricing/xai.toml");

/// Vendor discriminator. Defined here (not in `ccteam-core::harness`)
/// so this crate has zero `ccteam-core` deps; `ccteam-core` re-exports
/// `ccteam_cost::Vendor` from its `lib.rs` for downstream callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Vendor {
    Claude,
    Codex,
    Grok,
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
}

static ANTHROPIC_TABLE: OnceLock<PricingTable> = OnceLock::new();
static OPENAI_TABLE: OnceLock<PricingTable> = OnceLock::new();
static XAI_TABLE: OnceLock<PricingTable> = OnceLock::new();

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
        Vendor::Grok => XAI_TABLE.get_or_init(|| {
            toml::from_str(XAI_TOML).expect("ccteam xai.toml embedded at compile time must parse")
        }),
    }
}

/// Compute the dollar cost of one usage block.
///
/// Returns `None` when `model` (after stripping the optional `[1m]`
/// 1M-context suffix Anthropic attaches) is **not** a key in the vendor
/// table — there is no silent fallback to another model's rate. The miss
/// emits a single WARN per process per `(vendor, model)` so the unknown
/// id surfaces in the logs; the caller renders the absent cost as "—" /
/// excludes it from a sum. The matcher is permissive only on the `[1m]`
/// suffix (so `claude-opus-4-8[1m]` resolves to `claude-opus-4-8`).
pub fn estimate_cost(usage: &UnifiedTokenUsage, vendor: Vendor, model: &str) -> Option<f64> {
    let prices = resolve(vendor, model)?;
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
    Some(
        (from_input + from_cached + from_output + from_cache_create + from_reasoning) / 1_000_000.0,
    )
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
/// tries the raw id; returns `None` (WARN-once) on a miss — no fallback
/// to another model's rate.
fn resolve(vendor: Vendor, model: &str) -> Option<ModelPrices> {
    let normalized = normalize_model_id(model);
    let tbl = table_for(vendor);
    if let Some(p) = tbl.models.get(&normalized) {
        return Some(*p);
    }
    warn_unknown_model_once(vendor, &normalized);
    None
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
            "unknown model id; cost is unpriced (shown as \"—\", excluded from sums). Add this model to the ccteam pricing table to price it.",
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
        )
        .expect("known model prices");
        assert!(cost.abs() < 1e-12);
    }

    #[test]
    fn normalize_strips_1m_suffix() {
        assert_eq!(normalize_model_id("claude-opus-4-7[1m]"), "claude-opus-4-7");
        assert_eq!(normalize_model_id("claude-sonnet-4-6"), "claude-sonnet-4-6");
    }

    #[test]
    fn estimate_cost_unknown_claude_model_is_none() {
        // No silent fallback: an unknown model exposes itself as `None`
        // (rendered "—" / excluded from sums), never billed at sonnet's rate.
        let cost = estimate_cost(
            &UnifiedTokenUsage {
                input_tokens: 1_000_000,
                ..Default::default()
            },
            Vendor::Claude,
            "claude-future-99",
        );
        assert!(
            cost.is_none(),
            "unknown model must price to None, got {cost:?}"
        );
    }

    #[test]
    fn estimate_cost_synthetic_model_is_none() {
        // The transcript writes a `<synthetic>` model id for some internal
        // turns; it is not a real billable model → must price to None.
        let cost = estimate_cost(
            &UnifiedTokenUsage {
                output_tokens: 1_000_000,
                ..Default::default()
            },
            Vendor::Claude,
            "<synthetic>",
        );
        assert!(
            cost.is_none(),
            "<synthetic> must price to None, got {cost:?}"
        );
    }

    #[test]
    fn estimate_cost_empty_model_is_none() {
        // The legacy `""` escape hatch is gone — an empty model is unknown.
        assert!(estimate_cost(
            &UnifiedTokenUsage {
                output_tokens: 1,
                ..Default::default()
            },
            Vendor::Codex,
            "",
        )
        .is_none());
    }

    // -- new in V0.6.0 Wave 1 --------------------------------------------

    #[test]
    fn estimate_cost_codex_o3_input_matches_table() {
        let one_m_input = UnifiedTokenUsage {
            input_tokens: 1_000_000,
            ..Default::default()
        };
        // o3 input = $2 / 1M.
        let cost = estimate_cost(&one_m_input, Vendor::Codex, "o3").expect("o3 priced");
        assert!(
            (cost - 2.0).abs() < 0.01,
            "o3 input != $2 / 1M (got {cost})"
        );
    }

    #[test]
    fn estimate_cost_codex_reasoning_billed_as_output() {
        // o3 reasoning = $8 / 1M (== output rate).
        let usage = UnifiedTokenUsage {
            reasoning_output_tokens: Some(1_000_000),
            ..Default::default()
        };
        let cost = estimate_cost(&usage, Vendor::Codex, "o3").expect("o3 priced");
        assert!(
            (cost - 8.0).abs() < 0.01,
            "o3 reasoning != $8 / 1M (got {cost})",
        );
    }

    #[test]
    fn estimate_cost_codex_unknown_model_is_none() {
        // No fallback: an imaginary Codex model exposes as None.
        let usage = UnifiedTokenUsage {
            input_tokens: 1_000_000,
            ..Default::default()
        };
        let cost = estimate_cost(&usage, Vendor::Codex, "o9-imaginary");
        assert!(
            cost.is_none(),
            "unknown codex model must be None, got {cost:?}"
        );
    }

    #[test]
    fn dual_vendor_pricing_isolated() {
        // Same usage block, different vendor → different cost.
        let usage = UnifiedTokenUsage {
            input_tokens: 1_000_000,
            ..Default::default()
        };
        let claude = estimate_cost(&usage, Vendor::Claude, "claude-sonnet-4-6").unwrap(); // $3
        let codex = estimate_cost(&usage, Vendor::Codex, "o3").unwrap(); // $2
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
        let cost = estimate_cost(&usage, Vendor::Claude, "claude-sonnet-4-6").unwrap();
        assert!(
            (cost - 3.75).abs() < 0.01,
            "sonnet-4-6 cache_creation != $3.75 / 1M (got {cost})",
        );
    }

    #[test]
    fn opus_4_8_is_priced_not_sonnet_fallback() {
        // The owner's primary model. It MUST have its own row — it was missing,
        // so usage was silently billed at the sonnet-4-6 fallback ($15/1M out).
        // 1M output @ opus-4-8 = $25; the `[1m]` 1M-context suffix strips to the
        // same row.
        let m_out = UnifiedTokenUsage {
            output_tokens: 1_000_000,
            ..Default::default()
        };
        let opus = estimate_cost(&m_out, Vendor::Claude, "claude-opus-4-8").unwrap();
        assert!(
            (opus - 25.0).abs() < 0.01,
            "opus-4-8 output != $25 / 1M (got {opus}) — is it in the table?",
        );
        let opus_1m = estimate_cost(&m_out, Vendor::Claude, "claude-opus-4-8[1m]").unwrap();
        assert!(
            (opus_1m - 25.0).abs() < 0.01,
            "opus-4-8[1m] must strip to the same $25 row (got {opus_1m})",
        );
    }

    #[test]
    fn gpt_5_5_priced_and_unknown_codex_is_none() {
        // gpt-5.5 = current Codex default. 1M output = $30.
        let m_out = UnifiedTokenUsage {
            output_tokens: 1_000_000,
            ..Default::default()
        };
        let g = estimate_cost(&m_out, Vendor::Codex, "gpt-5.5").unwrap();
        assert!(
            (g - 30.0).abs() < 0.01,
            "gpt-5.5 output != $30 / 1M (got {g})"
        );
        // An unknown / unpriced Codex model (e.g. gpt-5.3-codex-spark, no public
        // price) now prices to None — exposed, not billed at another rate.
        let spark = estimate_cost(&m_out, Vendor::Codex, "gpt-5.3-codex-spark");
        assert!(
            spark.is_none(),
            "unpriced codex model must be None (exposed), got {spark:?}",
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
