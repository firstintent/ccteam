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

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::pricing::Vendor;

/// Per-advisor-call cost estimate used by advise budget ledger rows.
/// Kept intentionally approximate: both Claude text mode and Codex
/// JSONL advisors are capped to small prompts, so a flat estimate keeps
/// budget enforcement deterministic without vendor-specific usage
/// parsing.
pub const APPROX_COST_PER_CALL_USD: f64 = 0.005;

/// Fallback rolling 24h cap for advise calls when no explicit cap is
/// supplied.
pub const DEFAULT_ADVISE_BUDGET_USD_24H: f64 = 0.50;

/// Per-vendor budget pair. Either side can be omitted in YAML and
/// defaults to `BudgetCap::default()` (no caps).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Budgets {
    #[serde(default, skip_serializing_if = "BudgetCap::is_empty")]
    pub claude: BudgetCap,
    #[serde(default, skip_serializing_if = "BudgetCap::is_empty")]
    pub codex: BudgetCap,
    #[serde(default, skip_serializing_if = "BudgetCap::is_empty")]
    pub grok: BudgetCap,
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
            Vendor::Grok => &self.grok,
        }
    }

    /// Sum of per-vendor `max_cost_usd_per_24h` values. Used by the F84
    /// watchdog when vendors are configured — tripping the aggregated
    /// cap is the worst-case "shut everything down" path.
    /// Returns `None` only when **no** vendor has a cap.
    pub fn aggregated_cost_cap_24h(&self) -> Option<f64> {
        let caps = [
            self.claude.max_cost_usd_per_24h,
            self.codex.max_cost_usd_per_24h,
            self.grok.max_cost_usd_per_24h,
        ];
        let sum: f64 = caps.iter().filter_map(|c| *c).sum();
        if caps.iter().any(|c| c.is_some()) {
            Some(sum)
        } else {
            None
        }
    }
}

/// Persistent advise-budget ledger written to
/// `<ccteam_root>/cost-budget.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AdviseBudgetLedger {
    /// Rolling-window samples. Each entry = one advise call
    /// (vendor + cost USD + UTC ts). The 24h sum is recomputed on
    /// every check.
    #[serde(default)]
    pub samples: Vec<BudgetSample>,
}

/// One ledger row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BudgetSample {
    #[serde(with = "vendor_wire")]
    pub vendor: Vendor,
    pub usd: f64,
    pub ts: DateTime<Utc>,
}

/// Errors surfaced by cost-budget ledger persistence.
#[derive(Debug, Error)]
pub enum BudgetLedgerError {
    #[error("io error: {0}")]
    Io(String),
    #[error("serialize ledger: {0}")]
    Serialize(String),
}

/// Resolve the ledger file path under a ccteam-root directory.
pub fn budget_ledger_path(ccteam_root: &Path) -> PathBuf {
    ccteam_root.join("cost-budget.json")
}

/// Read the ledger, returning an empty one on missing / malformed file
/// (we never want budget reads to fail loud; recovery is automatic on
/// the next write).
pub fn load_budget_ledger(ccteam_root: &Path) -> Result<AdviseBudgetLedger, BudgetLedgerError> {
    let path = budget_ledger_path(ccteam_root);
    if !path.exists() {
        return Ok(AdviseBudgetLedger::default());
    }
    let body = std::fs::read_to_string(&path)
        .map_err(|e| BudgetLedgerError::Io(format!("read {}: {e}", path.display())))?;
    let ledger: AdviseBudgetLedger = serde_json::from_str(&body).unwrap_or_default();
    Ok(ledger)
}

/// Append one sample, atomic-rename the ledger. We GC samples older
/// than 48h on every write so the file stays bounded even under
/// many-calls-per-day workloads.
pub fn append_budget_sample(
    ccteam_root: &Path,
    vendor: Vendor,
    usd: f64,
) -> Result<(), BudgetLedgerError> {
    std::fs::create_dir_all(ccteam_root)
        .map_err(|e| BudgetLedgerError::Io(format!("mkdir {}: {e}", ccteam_root.display())))?;
    let mut ledger = load_budget_ledger(ccteam_root)?;
    ledger.samples.push(BudgetSample {
        vendor,
        usd,
        ts: Utc::now(),
    });
    let cutoff = Utc::now() - chrono::Duration::hours(48);
    ledger.samples.retain(|s| s.ts >= cutoff);
    let body = serde_json::to_string_pretty(&ledger)
        .map_err(|e| BudgetLedgerError::Serialize(e.to_string()))?;
    let path = budget_ledger_path(ccteam_root);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body.as_bytes())
        .map_err(|e| BudgetLedgerError::Io(format!("write {}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        BudgetLedgerError::Io(format!(
            "rename {} -> {}: {e}",
            tmp.display(),
            path.display()
        ))
    })?;
    Ok(())
}

/// Alias for adapter call sites that record a vendor turn rather than
/// an advise tool fan-out slot.
pub fn append_budget_ledger_row(
    ccteam_root: &Path,
    vendor: Vendor,
    usd: f64,
) -> Result<(), BudgetLedgerError> {
    append_budget_sample(ccteam_root, vendor, usd)
}

/// Sum advise spend over the last 24h (regardless of vendor).
pub fn sum_advise_today(ledger: &AdviseBudgetLedger) -> f64 {
    let cutoff = Utc::now() - chrono::Duration::hours(24);
    ledger
        .samples
        .iter()
        .filter(|s| s.ts >= cutoff)
        .map(|s| s.usd)
        .sum()
}

/// Sum advise spend for one vendor over the last 24h.
pub fn sum_advise_today_by_vendor(ledger: &AdviseBudgetLedger, vendor: Vendor) -> f64 {
    let cutoff = Utc::now() - chrono::Duration::hours(24);
    ledger
        .samples
        .iter()
        .filter(|s| s.ts >= cutoff && s.vendor == vendor)
        .map(|s| s.usd)
        .sum()
}

mod vendor_wire {
    use serde::{de::Error as _, Deserialize, Deserializer, Serializer};

    use crate::pricing::Vendor;

    pub fn serialize<S>(vendor: &Vendor, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(match vendor {
            Vendor::Claude => "claude",
            Vendor::Codex => "codex",
            Vendor::Grok => "grok",
        })
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vendor, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        match raw.to_ascii_lowercase().as_str() {
            "claude" => Ok(Vendor::Claude),
            "codex" => Ok(Vendor::Codex),
            "grok" => Ok(Vendor::Grok),
            other => Err(D::Error::custom(format!("unknown vendor {other:?}"))),
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
        };
        assert_eq!(b.cap_for(Vendor::Claude).max_cost_usd_per_24h, Some(5.0));
        assert_eq!(
            b.cap_for(Vendor::Codex).max_agent_spawns_per_hour,
            Some(100)
        );
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

    #[test]
    fn advise_budget_ledger_roundtrips_lowercase_vendor_wire() {
        let ledger = AdviseBudgetLedger {
            samples: vec![BudgetSample {
                vendor: Vendor::Codex,
                usd: 0.005,
                ts: Utc::now(),
            }],
        };
        let body = serde_json::to_string(&ledger).unwrap();
        assert!(body.contains("\"vendor\":\"codex\""));
        let back: AdviseBudgetLedger = serde_json::from_str(&body).unwrap();
        assert_eq!(back.samples[0].vendor, Vendor::Codex);
    }

    #[test]
    fn advise_budget_ledger_reads_legacy_pascal_vendor_wire() {
        let body = r#"{"samples":[{"vendor":"Claude","usd":0.01,"ts":"2026-05-31T00:00:00Z"}]}"#;
        let ledger: AdviseBudgetLedger = serde_json::from_str(body).unwrap();
        assert_eq!(ledger.samples[0].vendor, Vendor::Claude);
    }

    #[test]
    fn advise_budget_ledger_gc_drops_stale_samples_on_next_write() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        let stale_ts = Utc::now() - chrono::Duration::hours(49);
        let ledger = AdviseBudgetLedger {
            samples: vec![BudgetSample {
                vendor: Vendor::Claude,
                usd: 99.0,
                ts: stale_ts,
            }],
        };
        std::fs::write(
            budget_ledger_path(root),
            serde_json::to_string_pretty(&ledger).unwrap(),
        )
        .unwrap();

        append_budget_sample(root, Vendor::Claude, 0.005).unwrap();
        let ledger = load_budget_ledger(root).unwrap();
        assert_eq!(ledger.samples.len(), 1, "stale sample must be GC'd");
        assert!(
            (sum_advise_today(&ledger) - 0.005).abs() < 1e-9,
            "24h sum reflects only the fresh sample"
        );
    }
}
