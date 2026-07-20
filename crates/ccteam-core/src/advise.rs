//! **REMOVED product surface (v0.8.24 C2)** — `advise_vote` /
//! `advise_parallel` are gone. MCP tools were culled in v0.9-T1.
//!
//! This module keeps thin re-exports of the shared **budget ledger** helpers
//! still used by `codex_exec` critic accounting and doctor cost-orphan
//! rollups (`ccteam_cost::{load_budget_ledger, append_budget_sample, …}`).

use std::path::Path;

pub use ccteam_cost::{
    append_budget_sample, budget_ledger_path, load_budget_ledger, sum_advise_today,
    sum_advise_today_by_vendor, AdviseBudgetLedger, BudgetSample, APPROX_COST_PER_CALL_USD,
    DEFAULT_ADVISE_BUDGET_USD_24H,
};

use crate::AgentVendor;

/// Record one ledger sample. Accepts [`AgentVendor`] (converts to the cost
/// crate's `Vendor`) so doctor/adapter call sites stay on the harness enum.
pub fn append_budget_ledger_row(
    ccteam_root: &Path,
    vendor: AgentVendor,
    usd: f64,
) -> Result<(), String> {
    ccteam_cost::append_budget_ledger_row(ccteam_root, vendor.cost_vendor(), usd)
        .map_err(|e| e.to_string())
}

/// Typed alias for [`append_budget_sample`] used by the codex critic path
/// (records USD against the advise budget ledger file).
pub fn append_budget_sample_for_vendor(
    ccteam_root: &Path,
    vendor: AgentVendor,
    usd: f64,
) -> Result<(), String> {
    append_budget_sample(ccteam_root, vendor.cost_vendor(), usd).map_err(|e| e.to_string())
}

/// Sum of ledger samples for one vendor in the rolling 24h window.
pub fn sum_advise_today_by_agent_vendor(ledger: &AdviseBudgetLedger, vendor: AgentVendor) -> f64 {
    sum_advise_today_by_vendor(ledger, vendor.cost_vendor())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn ledger_round_trip_sample() {
        let tmp = TempDir::new().unwrap();
        append_budget_sample(tmp.path(), ccteam_cost::Vendor::Claude, 0.01).unwrap();
        let ledger = load_budget_ledger(tmp.path()).unwrap();
        assert!((sum_advise_today(&ledger) - 0.01).abs() < 1e-9);
    }
}
