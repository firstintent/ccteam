//! V0.6.6 F173 — `ccteam doctor --check-cost-orphan` invariant test.
//!
//! Cost-orphan = a vendor adapter spawn that produced a `progress.jsonl`
//! `agent_done` row but no matching `cost-budget.json` ledger row in
//! the same 24h window. Each vendor that fails the parity surfaces as
//! one `[WARN] cost orphan: …` line. Fully reconciled state emits
//! `[OK] cost ledger reconciled`.
//!
//! Tests drive the `ccteam doctor --check-cost-orphan` binary surface
//! with `CCTEAM_HOME` pointing at an ephemeral tempdir (real project
//! registered via on-disk `config.yaml` + `state.json`; ledger seeded
//! via on-disk `cost-budget.json`). This pins the operator-facing
//! report text for host probe / CI greps.

use ccteam_core::advise::{
    append_budget_ledger_row, budget_ledger_path, AdviseBudgetLedger, BudgetSample,
};
use ccteam_core::state::ProjectState;
use ccteam_core::Vendor;
use ccteam_harness::AgentVendor;
use chrono::Utc;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Materialise `<home>/.ccteam` with one registered project, an empty
/// `progress.jsonl`, and an empty `cost-budget.json`. Return
/// `(home_tempdir, ccteam_root, project_dir, slug)`.
fn ephemeral_home_with_one_project(slug: &str) -> (tempfile::TempDir, PathBuf, PathBuf, String) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join(".ccteam");
    let projects_root = tmp.path().join("projects");
    let project_dir = projects_root.join(slug);
    std::fs::create_dir_all(project_dir.join(".ccteam")).unwrap();
    std::fs::create_dir_all(root.join("progress")).unwrap();

    // config.yaml — register the project so `collect_projects` finds it.
    let cfg = serde_yaml::Value::Mapping({
        let mut m = serde_yaml::Mapping::new();
        m.insert(
            "projects".into(),
            serde_yaml::Value::Sequence(vec![{
                let mut p = serde_yaml::Mapping::new();
                p.insert("slug".into(), slug.into());
                p.insert(
                    "path".into(),
                    project_dir.to_string_lossy().to_string().into(),
                );
                serde_yaml::Value::Mapping(p)
            }]),
        );
        m
    });
    std::fs::write(
        root.join("config.yaml"),
        serde_yaml::to_string(&cfg).unwrap(),
    )
    .unwrap();

    // project/.ccteam/state.json — collect_projects loads via
    // `ProjectState::load`, which requires the full struct shape. Use
    // the API constructor + serde to write a valid file.
    let state = ProjectState::initial(slug.into());
    std::fs::write(
        project_dir.join(".ccteam").join("state.json"),
        serde_json::to_string_pretty(&state).unwrap(),
    )
    .unwrap();

    (tmp, root, project_dir, slug.to_string())
}

/// Append one `agent_done` JSONL row to `<root>/progress/<slug>.jsonl`.
fn append_agent_done(root: &Path, slug: &str, vendor: &str, ts_offset_secs: i64) {
    let ts = Utc::now() - chrono::Duration::seconds(ts_offset_secs);
    let row = json!({
        "event": "agent_done",
        "role": "test-role",
        "session_id": format!("sid-{}-{}", vendor, ts.timestamp_nanos_opt().unwrap_or(0)),
        "status": "completed",
        "cost_usd": 0.005,
        "vendor": vendor,
        "slug": slug,
        "ts": ts.to_rfc3339(),
    });
    let path = root.join("progress").join(format!("{slug}.jsonl"));
    let mut existing = std::fs::read_to_string(&path).unwrap_or_default();
    existing.push_str(&serde_json::to_string(&row).unwrap());
    existing.push('\n');
    std::fs::write(&path, existing).unwrap();
}

/// Run `ccteam doctor --check-cost-orphan` against the supplied root.
/// Returns `(stdout, exit_code)`.
fn run_doctor(ccteam_home: &Path, projects_root: &Path) -> (String, i32) {
    let bin = env!("CARGO_BIN_EXE_ccteam");
    let out = Command::new(bin)
        .env("CCTEAM_HOME", ccteam_home)
        .env("CCTEAM_PROJECTS_ROOT", projects_root)
        .arg("doctor")
        .arg("--check-cost-orphan")
        .output()
        .expect("spawn ccteam doctor");
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        out.status.code().unwrap_or(-1),
    )
}

/// 1. Empty ledger + empty progress.jsonl → reconciled, OK line.
#[test]
fn no_calls_no_ledger_is_reconciled() {
    let (tmp, root, project_dir, _slug) = ephemeral_home_with_one_project("orphan-empty");
    let projects_root = project_dir.parent().unwrap();
    let (stdout, code) = run_doctor(&root, projects_root);
    assert_eq!(code, 0, "doctor must exit 0. stdout:\n{stdout}");
    assert!(
        stdout.contains("[OK] cost ledger reconciled"),
        "missing OK line. stdout:\n{stdout}"
    );
    drop(tmp);
}

/// 2. Matched codex events + ledger rows → reconciled.
#[test]
fn matched_progress_and_ledger_is_reconciled() {
    let (tmp, root, project_dir, slug) = ephemeral_home_with_one_project("orphan-match");
    for _ in 0..3 {
        append_agent_done(&root, &slug, "codex", 60);
        append_budget_ledger_row(&root, AgentVendor::Codex, 0.005).unwrap();
    }
    let (stdout, code) = run_doctor(&root, project_dir.parent().unwrap());
    assert_eq!(code, 0);
    assert!(
        stdout.contains("[OK] cost ledger reconciled"),
        "expected OK reconciled. stdout:\n{stdout}"
    );
    // Counts line surfaces both numbers.
    assert!(
        stdout.contains("codex=3"),
        "expected codex=3 line. stdout:\n{stdout}"
    );
    drop(tmp);
}

/// 3. Codex orphan → WARN line surfaces.
#[test]
fn codex_orphan_surfaces_warning() {
    let (tmp, root, project_dir, slug) = ephemeral_home_with_one_project("orphan-codex");
    append_agent_done(&root, &slug, "codex", 60);
    append_agent_done(&root, &slug, "codex", 120);
    let (stdout, _) = run_doctor(&root, project_dir.parent().unwrap());
    assert!(
        stdout.contains("[WARN] cost orphan:"),
        "missing WARN line. stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("codex"),
        "WARN line should name codex. stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("Δ=2"),
        "WARN line should include delta. stdout:\n{stdout}"
    );
    drop(tmp);
}

/// 4. Mixed: claude reconciled, codex orphaned → only codex warning.
#[test]
fn mixed_only_orphaned_vendor_warns() {
    let (tmp, root, project_dir, slug) = ephemeral_home_with_one_project("orphan-mixed");
    append_agent_done(&root, &slug, "claude", 60);
    append_budget_ledger_row(&root, AgentVendor::Claude, 0.005).unwrap();
    append_agent_done(&root, &slug, "codex", 60);
    // NO ledger row for codex — simulating the F173 regression.
    let (stdout, _) = run_doctor(&root, project_dir.parent().unwrap());
    // Find WARN lines.
    let warns: Vec<&str> = stdout
        .lines()
        .filter(|l| l.contains("[WARN] cost orphan"))
        .collect();
    assert_eq!(
        warns.len(),
        1,
        "expected exactly one WARN. stdout:\n{stdout}"
    );
    assert!(warns[0].contains("codex"), "WARN should be for codex");
    assert!(
        !warns[0].contains("claude"),
        "WARN should not mention claude"
    );
    drop(tmp);
}

/// 5. Stale events (>24h) are excluded.
#[test]
fn stale_progress_events_excluded_from_window() {
    let (tmp, root, project_dir, slug) = ephemeral_home_with_one_project("orphan-stale");
    append_agent_done(&root, &slug, "codex", 25 * 3600); // 25h ago
    let (stdout, _) = run_doctor(&root, project_dir.parent().unwrap());
    assert!(
        stdout.contains("[OK] cost ledger reconciled"),
        "stale events must be filtered. stdout:\n{stdout}"
    );
    drop(tmp);
}

/// 6. Ledger-only rows (advise_vote without progress.jsonl footprint)
///    must NOT surface as orphans — the invariant is one-directional.
#[test]
fn ledger_only_rows_do_not_count_as_orphans() {
    let (tmp, root, project_dir, _slug) = ephemeral_home_with_one_project("orphan-ledger-only");
    for _ in 0..5 {
        append_budget_ledger_row(&root, AgentVendor::Codex, 0.005).unwrap();
    }
    let (stdout, _) = run_doctor(&root, project_dir.parent().unwrap());
    assert!(
        stdout.contains("[OK] cost ledger reconciled"),
        "ledger-only rows must not orphan-warn. stdout:\n{stdout}"
    );
    drop(tmp);
}

/// 7. Report header text is stable (host probes grep for it).
#[test]
fn report_header_text_stable() {
    let (tmp, root, project_dir, _slug) = ephemeral_home_with_one_project("orphan-header");
    let (stdout, _) = run_doctor(&root, project_dir.parent().unwrap());
    assert!(
        stdout.contains("ccteam doctor --check-cost-orphan"),
        "report header text changed — coordinate with downstream grep. stdout:\n{stdout}"
    );
    drop(tmp);
}

/// 8. Pre-seeded large ledger does not cause a warning (we only check
///    `agent_done > ledger_rows`, not the reverse).
#[test]
fn fully_seeded_ledger_via_hand_write_round_trips() {
    let (tmp, root, project_dir, _slug) = ephemeral_home_with_one_project("orphan-handwrite");
    let ledger = AdviseBudgetLedger {
        samples: vec![
            BudgetSample {
                vendor: Vendor::Codex,
                usd: 0.005,
                ts: Utc::now(),
            },
            BudgetSample {
                vendor: Vendor::Claude,
                usd: 0.005,
                ts: Utc::now(),
            },
        ],
    };
    std::fs::write(
        budget_ledger_path(&root),
        serde_json::to_string_pretty(&ledger).unwrap(),
    )
    .unwrap();
    let (stdout, _) = run_doctor(&root, project_dir.parent().unwrap());
    assert!(
        stdout.contains("codex=1"),
        "ledger codex row should appear. stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("claude=1"),
        "ledger claude row should appear. stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("[OK] cost ledger reconciled"),
        "should be reconciled. stdout:\n{stdout}"
    );
    drop(tmp);
}
