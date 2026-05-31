//! V0.6.6 F169 — `@ccteam cost today` now reads the real
//! `<ccteam_root>/cost-budget.json` advise ledger (V0.6.5 F152 schema)
//! instead of returning the V0.6.1 bot-count placeholder. These tests
//! pin the format + numeric correctness across four ledger shapes:
//!
//! 1. empty ledger        → header + 0.0000 totals, no warning prefix
//! 2. single-vendor spend → Claude row only, total = Claude row
//! 3. dual-vendor spend   → both rows + total = sum
//! 4. >80% of cap         → ⚠️ approaching daily budget cap prefix
//!
//! The executor is constructed with `with_ccteam_root` so each test
//! owns its ledger file in a `TempDir` — no `HOME` env mutation race
//! with the other integration tests in this crate.

use ccteam_core::advise::{append_budget_sample, DEFAULT_ADVISE_BUDGET_USD_24H};
use ccteam_harness::AgentVendor;
use ccteam_im::nl_admin::{AdminCmd, AdminExecutor, AdminSideEffect};
use tempfile::TempDir;

/// Build an executor wired to a fresh `(projects_root, ccteam_root)`
/// pair so each test owns its ledger state.
fn build_executor() -> (TempDir, TempDir, AdminExecutor) {
    let projects = TempDir::new().unwrap();
    let ccteam = TempDir::new().unwrap();
    let exec = AdminExecutor::new(projects.path()).with_ccteam_root(ccteam.path());
    (projects, ccteam, exec)
}

async fn run_cost_today(exec: &AdminExecutor) -> (AdminSideEffect, String) {
    let reply = exec
        .execute(AdminCmd::CostToday { slug: None }, "@group-test")
        .await;
    (reply.side_effect, reply.message)
}

#[tokio::test]
async fn cost_today_empty_ledger_reports_zeros_no_warning() {
    let (_p, _c, exec) = build_executor();
    let (side, msg) = run_cost_today(&exec).await;
    assert_eq!(side, AdminSideEffect::None);
    assert!(msg.contains("ccteam cost today"), "header missing: {msg}");
    assert!(msg.contains("Claude $0.0000"), "claude zero missing: {msg}");
    assert!(msg.contains("Codex $0.0000"), "codex zero missing: {msg}");
    assert!(msg.contains("total $0.0000"), "total zero missing: {msg}");
    assert!(
        msg.contains(&format!("cap: ${DEFAULT_ADVISE_BUDGET_USD_24H:.2}/24h")),
        "cap line missing: {msg}"
    );
    assert!(
        !msg.contains("approaching daily budget cap"),
        "warning must NOT trigger on empty ledger: {msg}"
    );
}

#[tokio::test]
async fn cost_today_single_vendor_spend_reports_claude_only() {
    let (_p, ccteam, exec) = build_executor();
    // Seed two Claude calls; no Codex rows.
    append_budget_sample(ccteam.path(), AgentVendor::Claude, 0.0123).unwrap();
    append_budget_sample(ccteam.path(), AgentVendor::Claude, 0.0077).unwrap();
    let (_, msg) = run_cost_today(&exec).await;
    assert!(
        msg.contains("Claude $0.0200"),
        "claude sum 0.02 expected: {msg}"
    );
    assert!(msg.contains("Codex $0.0000"), "codex zero expected: {msg}");
    assert!(msg.contains("total $0.0200"), "total 0.02 expected: {msg}");
    assert!(!msg.contains("approaching daily budget cap"));
}

#[tokio::test]
async fn cost_today_dual_vendor_spend_sums_both() {
    let (_p, ccteam, exec) = build_executor();
    append_budget_sample(ccteam.path(), AgentVendor::Claude, 0.0500).unwrap();
    append_budget_sample(ccteam.path(), AgentVendor::Codex, 0.0300).unwrap();
    let (_, msg) = run_cost_today(&exec).await;
    assert!(msg.contains("Claude $0.0500"), "{msg}");
    assert!(msg.contains("Codex $0.0300"), "{msg}");
    assert!(msg.contains("total $0.0800"), "{msg}");
    // 0.08 / 0.50 = 16% — well below the 80% warning threshold.
    assert!(!msg.contains("approaching daily budget cap"));
}

#[tokio::test]
async fn cost_today_emits_warning_above_eighty_percent_cap() {
    let (_p, ccteam, exec) = build_executor();
    // Seed 0.45 USD = 90% of the 0.50 default cap.
    for _ in 0..9 {
        append_budget_sample(ccteam.path(), AgentVendor::Claude, 0.05).unwrap();
    }
    let (_, msg) = run_cost_today(&exec).await;
    assert!(
        msg.starts_with("⚠️ approaching daily budget cap"),
        "warning prefix missing: {msg}"
    );
    assert!(msg.contains("total $0.4500"), "{msg}");
    // Remaining shown as cap − total = 0.05.
    assert!(msg.contains("remaining: $0.0500"), "remaining line: {msg}");
}
