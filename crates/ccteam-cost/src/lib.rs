//! V0.6.0 Wave 1 — dual-vendor pricing + cost classification + budget caps.
//!
//! Extracted from `ccteam-core` so Anthropic Claude and OpenAI Codex
//! share one pricing surface (`UnifiedTokenUsage` + `estimate_cost`)
//! and one budget shape (`Budgets { claude, codex }`). The crate has
//! **no dependency on `ccteam-core`** — `classify()` takes primitives,
//! not `ProjectState` — so the dep graph runs strictly cost → core,
//! never the reverse.
//!
//! ## Layout
//!
//! - `pricing` — `UnifiedTokenUsage`, `ModelPrices`, `Vendor`,
//!   `estimate_cost`, `pricing_schema_version`. Two embedded TOML tables
//!   (`pricing/anthropic.toml`, `pricing/openai.toml`) loaded once via
//!   `OnceLock`; unknown model ids fall back per-vendor with WARN-once.
//! - `level` — `CostLevel` + `classify(cost, soft, hard)` for the F84
//!   budget-cap watchdog.
//! - `budget` — `Budgets { claude: BudgetCap, codex: BudgetCap }` —
//!   per-vendor cost / spawn caps. The V0.5.x flat `BudgetSpec` lives
//!   in `ccteam-core::workflow` and feeds the legacy claude path;
//!   V0.6 workflows can opt into per-vendor split via
//!   `WorkflowSpec::budgets_v060`.

pub mod budget;
pub mod level;
pub mod pricing;

pub use budget::{BudgetCap, Budgets};
pub use level::{classify, CostLevel, COST_MID_WARN_USD};
pub use pricing::{
    estimate_cost, pricing_schema_version, ModelPrices, UnifiedTokenUsage, Vendor,
};
