//! V0.6.0 Wave 3 F112 — per-vendor budget caps: queries::compute_cost_summary
//! splits 24h cost by vendor and the orchestrator's enforce_budget
//! reads `budgets_v060.{claude,codex}.max_cost_usd_per_24h` independently.
//!
//! These tests exercise the pure-event-slice helper directly (no
//! orchestrator) so they stay deterministic + hermetic.

use ccteam_core::queries::compute_cost_summary;
use chrono::Utc;
use serde_json::json;

fn done_event(vendor: Option<&str>, cost: f64, sid: &str) -> serde_json::Value {
    let ts = Utc::now().to_rfc3339();
    let mut e = json!({
        "event": "agent_done",
        "role": "coder",
        "session_id": sid,
        "status": "completed",
        "cost_usd": cost,
        "slug": "demo",
        "ts": ts,
    });
    if let Some(v) = vendor {
        e["vendor"] = serde_json::Value::String(v.to_string());
    }
    e
}

#[test]
fn cost_summary_splits_per_vendor_when_field_present() {
    let events = vec![
        done_event(Some("claude"), 1.25, "s1"),
        done_event(Some("codex"), 0.50, "s2"),
        done_event(Some("claude"), 0.75, "s3"),
    ];
    let summary = compute_cost_summary(&events, Utc::now(), |_| {
        ccteam_core::claude_job::JobLiveness::Terminal {
            status: "completed",
            cost_usd: 0.0,
        }
    });
    assert!((summary.cost_24h_usd - 2.5).abs() < 1e-9);
    assert_eq!(
        summary.cost_24h_by_vendor.get("claude").copied(),
        Some(2.0),
        "{:?}",
        summary.cost_24h_by_vendor
    );
    assert_eq!(summary.cost_24h_by_vendor.get("codex").copied(), Some(0.5));
}

#[test]
fn cost_summary_skips_vendor_split_when_field_missing() {
    // Pre-V0.6 events lacked the vendor field. They contribute to the
    // aggregate but the per-vendor map stays empty (per-vendor caps
    // are V0.6-only opt-in).
    let events = vec![
        done_event(None, 5.0, "s1"),
        done_event(None, 1.0, "s2"),
    ];
    let summary = compute_cost_summary(&events, Utc::now(), |_| {
        ccteam_core::claude_job::JobLiveness::Terminal {
            status: "completed",
            cost_usd: 0.0,
        }
    });
    assert!((summary.cost_24h_usd - 6.0).abs() < 1e-9);
    assert!(summary.cost_24h_by_vendor.is_empty());
}

#[test]
fn cost_summary_aggregates_total_per_vendor() {
    let events = vec![
        done_event(Some("claude"), 10.0, "s1"),
        done_event(Some("codex"), 3.0, "s2"),
        done_event(Some("claude"), 2.0, "s3"),
        done_event(Some("codex"), 1.0, "s4"),
    ];
    let summary = compute_cost_summary(&events, Utc::now(), |_| {
        ccteam_core::claude_job::JobLiveness::Terminal {
            status: "completed",
            cost_usd: 0.0,
        }
    });
    assert_eq!(summary.cost_total_by_vendor.get("claude").copied(), Some(12.0));
    assert_eq!(summary.cost_total_by_vendor.get("codex").copied(), Some(4.0));
    assert!((summary.cost_total_usd - 16.0).abs() < 1e-9);
}

#[test]
fn cost_summary_per_vendor_serialises_in_json() {
    // JSON shape contract: cost_24h_by_vendor + cost_total_by_vendor
    // must be present (default-empty) for SPA / API consumers.
    let summary = compute_cost_summary(&[], Utc::now(), |_| {
        ccteam_core::claude_job::JobLiveness::Terminal {
            status: "completed",
            cost_usd: 0.0,
        }
    });
    let j = serde_json::to_value(&summary).unwrap();
    assert!(j.get("cost_24h_by_vendor").is_some());
    assert!(j.get("cost_total_by_vendor").is_some());
    assert!(j["cost_24h_by_vendor"].is_object());
}

#[test]
fn budgets_aggregated_cap_uses_ccteam_cost_helper() {
    use ccteam_cost::{BudgetCap, Budgets};
    let b = Budgets {
        claude: BudgetCap {
            max_cost_usd_per_24h: Some(5.0),
            ..Default::default()
        },
        codex: BudgetCap {
            max_cost_usd_per_24h: Some(2.0),
            ..Default::default()
        },
    };
    assert_eq!(b.aggregated_cost_cap_24h(), Some(7.0));
}
