//! V0.6.0 Wave 1 — per-vendor budget caps.
//!
//! `Budgets` splits the V0.5.x flat `BudgetSpec` into one cap per
//! vendor so a workflow can run Claude + Codex against independent
//! ceilings (e.g. "$5/day on Claude, $2/day on Codex"). The V0.5.x
//! `BudgetSpec` survives as a legacy synonym for `Budgets.claude`;
//! Wave 2 / V0.7 deprecates the flat path.
//!
//! `Budgets` is **schema-only** here — the F84 watchdog evaluation
//! (sum `agent_done.cost_usd` over the rolling window, compare,
//! emit `budget_exceeded`) stays in `ccteam-core::queries` because
//! it depends on `progress.jsonl` parsing types.

use serde::{Deserialize, Serialize};

use crate::pricing::Vendor;

/// Per-vendor budget pair. Either side can be omitted in YAML and
/// defaults to `BudgetCap::default()` (no caps).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Budgets {
    #[serde(default, skip_serializing_if = "BudgetCap::is_empty")]
    pub claude: BudgetCap,
    #[serde(default, skip_serializing_if = "BudgetCap::is_empty")]
    pub codex: BudgetCap,
}

/// One vendor's cost + spawn caps.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BudgetCap {
    /// Rolling 24h window cost cap (USD). Sum of `agent_done.cost_usd`
    /// for events with `ts >= now - 24h` ≥ this value → trip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost_usd_per_24h: Option<f64>,
    /// Rolling 1h spawn count cap. Number of `agent_spawn` events with
    /// `ts >= now - 1h` ≥ this value → trip. Defends against
    /// self-excitation runaway.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_agent_spawns_per_hour: Option<u32>,
}

impl BudgetCap {
    pub fn is_empty(&self) -> bool {
        self.max_cost_usd_per_24h.is_none() && self.max_agent_spawns_per_hour.is_none()
    }
}

impl Budgets {
    /// Cap for one vendor. Lets the watchdog look up "what's the
    /// claude ceiling?" without a `match` at every call site.
    pub fn cap_for(&self, vendor: Vendor) -> &BudgetCap {
        match vendor {
            Vendor::Claude => &self.claude,
            Vendor::Codex => &self.codex,
        }
    }

    /// Sum of the two `max_cost_usd_per_24h` values. Used by the F84
    /// watchdog when both vendors are configured — tripping the
    /// aggregated cap is the worst-case "shut everything down" path.
    /// Returns `None` only when **neither** vendor has a cap.
    pub fn aggregated_cost_cap_24h(&self) -> Option<f64> {
        match (
            self.claude.max_cost_usd_per_24h,
            self.codex.max_cost_usd_per_24h,
        ) {
            (Some(c), Some(d)) => Some(c + d),
            (Some(c), None) => Some(c),
            (None, Some(d)) => Some(d),
            (None, None) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregated_cost_cap_sums_both_when_present() {
        let b = Budgets {
            claude: BudgetCap {
                max_cost_usd_per_24h: Some(5.0),
                max_agent_spawns_per_hour: None,
            },
            codex: BudgetCap {
                max_cost_usd_per_24h: Some(2.0),
                max_agent_spawns_per_hour: None,
            },
        };
        assert_eq!(b.aggregated_cost_cap_24h(), Some(7.0));
    }

    #[test]
    fn aggregated_cost_cap_returns_lone_side_when_only_one_set() {
        let b = Budgets {
            claude: BudgetCap {
                max_cost_usd_per_24h: Some(5.0),
                ..Default::default()
            },
            codex: BudgetCap::default(),
        };
        assert_eq!(b.aggregated_cost_cap_24h(), Some(5.0));
    }

    #[test]
    fn aggregated_cost_cap_none_when_both_unset() {
        let b = Budgets::default();
        assert!(b.aggregated_cost_cap_24h().is_none());
    }

    #[test]
    fn cap_for_returns_per_vendor_slice() {
        let b = Budgets {
            claude: BudgetCap {
                max_cost_usd_per_24h: Some(5.0),
                ..Default::default()
            },
            codex: BudgetCap {
                max_agent_spawns_per_hour: Some(100),
                ..Default::default()
            },
        };
        assert_eq!(b.cap_for(Vendor::Claude).max_cost_usd_per_24h, Some(5.0));
        assert_eq!(b.cap_for(Vendor::Codex).max_agent_spawns_per_hour, Some(100));
    }

    #[test]
    fn budget_cap_is_empty_when_both_fields_none() {
        assert!(BudgetCap::default().is_empty());
        assert!(!BudgetCap {
            max_cost_usd_per_24h: Some(1.0),
            ..Default::default()
        }
        .is_empty());
    }
}
