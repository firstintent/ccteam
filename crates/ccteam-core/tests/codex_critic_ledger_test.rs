//! V0.6.6 F173 — `CodexExecAdapter::submit_turn` advise-ledger hook
//! tests. Verifies the unified-cost-rollup invariant: every successful
//! Codex turn appends one row to `<ccteam_root>/cost-budget.json`, and
//! a pre-turn budget cap rejects further calls with a structured
//! `SubmitFailed("budget_exceeded: …")`.
//!
//! All tests use the `CCTEAM_CODEX_BIN` fake-script seam + a per-test
//! `CCTEAM_HOME` tempdir so they're hermetic — no real codex binary
//! required, no shared filesystem state.

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use ccteam_core::advise::{
    append_budget_ledger_row, load_budget_ledger, sum_advise_today, AdviseBudgetLedger,
    BudgetSample, APPROX_COST_PER_CALL_USD, DEFAULT_ADVISE_BUDGET_USD_24H,
};
use ccteam_core::AgentVendor;
use ccteam_core::Vendor;
use ccteam_harness::execution::CodexExecAdapter;
use ccteam_harness::{
    AgentVendor as HarnessAgentVendor, ExecutionMode, HarnessAdapter, HarnessError, ThreadEvent,
    ThreadHandle, TurnInput, CODEX_BIN_ENV,
};
use futures::StreamExt;
use serde_json::json;
use serial_test::serial;

/// Build a fake codex script that emits the supplied JSONL lines on
/// stdout and exits 0. Mirrors the fixture from
/// `codex_exec_wave3_test.rs` so failures don't cross-contaminate.
fn fake_codex_emitting(lines: &[&str]) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("codex.sh");
    let mut body = String::from("#!/usr/bin/env bash\n");
    body.push_str("cat <<'EOF'\n");
    for l in lines {
        body.push_str(l);
        body.push('\n');
    }
    body.push_str("EOF\n");
    body.push_str("exit 0\n");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        f.sync_all().unwrap();
    }
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    (dir, path)
}

fn handle(identity: &str) -> ThreadHandle {
    ThreadHandle {
        vendor: HarnessAgentVendor::Codex,
        mode: ExecutionMode::Bg,
        identity: identity.into(),
        started_at: chrono::Utc::now(),
        raw_extras: json!({}),
    }
}

/// One successful turn appends exactly one Codex ledger row at the
/// `APPROX_COST_PER_CALL_USD` flat estimate. We drain the event stream
/// for `TurnCompleted` before asserting so the post-turn ledger hook
/// has run.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn successful_turn_appends_one_codex_ledger_row() {
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("CCTEAM_HOME", home.path());

    let (_dir, script_path) = fake_codex_emitting(&[
        r#"{"type":"thread.started","thread_id":"t-led-1"}"#,
        r#"{"type":"turn.started"}"#,
        r#"{"type":"item.completed","item":{"id":"i-1","type":"agent_message","text":"hi"}}"#,
        r#"{"type":"turn.completed","usage":{"input_tokens":10,"output_tokens":5}}"#,
    ]);
    std::env::set_var(CODEX_BIN_ENV, &script_path);

    let adapter = CodexExecAdapter::new();
    let h = handle("ccteam-test-ledger-1");
    let stream = adapter.events(&h);
    let _tid = adapter
        .submit_turn(&h, TurnInput::UserText("hi".into()))
        .await
        .unwrap();

    // Drain until TurnCompleted so the post-turn ledger write has fired.
    let _ = tokio::time::timeout(
        Duration::from_secs(3),
        stream
            .take_while(|evt| {
                let stop = !matches!(evt, ThreadEvent::TurnCompleted { .. });
                std::future::ready(stop)
            })
            .collect::<Vec<_>>(),
    )
    .await
    .expect("stream drained");
    // The ledger row is appended *inside* the reader task right after
    // the TurnCompleted event is broadcast; give it a brief grace
    // period to flush to disk (no deeper sync point — the write is a
    // detached helper).
    tokio::time::sleep(Duration::from_millis(150)).await;

    let ledger = load_budget_ledger(home.path()).unwrap();
    let codex_rows: Vec<_> = ledger
        .samples
        .iter()
        .filter(|s| matches!(s.vendor, Vendor::Codex))
        .collect();
    assert_eq!(
        codex_rows.len(),
        1,
        "expected 1 codex row after one successful turn, got {} rows ({ledger:?})",
        codex_rows.len()
    );
    assert!((codex_rows[0].usd - APPROX_COST_PER_CALL_USD).abs() < 1e-9);

    std::env::remove_var(CODEX_BIN_ENV);
    std::env::remove_var("CCTEAM_HOME");
}

/// Failed turns (non-zero exit, no `turn.completed`) MUST NOT charge
/// the ledger — operators don't pay for failures, and `doctor
/// --check-cost-orphan` will naturally include only `status="completed"`
/// `agent_done` rows for parity.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn failed_turn_does_not_append_ledger_row() {
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("CCTEAM_HOME", home.path());

    // Fake script: exit 1, no JSONL output → fallback path emits
    // TurnFailed via "nonzero_exit".
    let dir = tempfile::tempdir().unwrap();
    let script_path = dir.path().join("codex_fail.sh");
    let body = "#!/usr/bin/env bash\nexit 1\n";
    {
        let mut f = std::fs::File::create(&script_path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        f.sync_all().unwrap();
    }
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script_path, perms).unwrap();

    std::env::set_var(CODEX_BIN_ENV, &script_path);
    let adapter = CodexExecAdapter::new();
    let h = handle("ccteam-test-ledger-fail");
    let stream = adapter.events(&h);
    let _ = adapter
        .submit_turn(&h, TurnInput::UserText("hi".into()))
        .await
        .unwrap();

    // Drain until TurnFailed
    let _ = tokio::time::timeout(
        Duration::from_secs(3),
        stream
            .take_while(|evt| std::future::ready(!matches!(evt, ThreadEvent::TurnFailed { .. })))
            .collect::<Vec<_>>(),
    )
    .await
    .expect("stream drained");
    tokio::time::sleep(Duration::from_millis(150)).await;

    let ledger = load_budget_ledger(home.path()).unwrap();
    let codex_rows: Vec<_> = ledger
        .samples
        .iter()
        .filter(|s| matches!(s.vendor, Vendor::Codex))
        .collect();
    assert!(
        codex_rows.is_empty(),
        "expected 0 codex rows after failed turn; got {codex_rows:?}",
    );

    std::env::remove_var(CODEX_BIN_ENV);
    std::env::remove_var("CCTEAM_HOME");
}

/// Pre-turn budget check rejects when the ledger is already at /
/// above cap. The error must be a `HarnessError::SubmitFailed` whose
/// message starts with `budget_exceeded:` so the orchestrator's
/// upstream pattern-match can detect it.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn submit_turn_rejects_when_budget_already_exceeded() {
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("CCTEAM_HOME", home.path());
    // Seed ledger past the default cap by hand-writing the file (we
    // don't want to depend on the public append helper here — that's
    // covered separately by advise.rs unit tests).
    let ledger = AdviseBudgetLedger {
        samples: vec![BudgetSample {
            vendor: Vendor::Codex,
            // 1.0 USD ≫ DEFAULT_ADVISE_BUDGET_USD_24H (0.50)
            usd: 1.0,
            ts: chrono::Utc::now(),
        }],
    };
    std::fs::create_dir_all(home.path()).unwrap();
    std::fs::write(
        home.path().join("cost-budget.json"),
        serde_json::to_string_pretty(&ledger).unwrap(),
    )
    .unwrap();

    // Fake codex script — should never be invoked because the pre-turn
    // gate fires first. Point at /bin/true as a safe no-op fallback.
    std::env::set_var(CODEX_BIN_ENV, "/bin/true");
    let adapter = CodexExecAdapter::new();
    let h = handle("ccteam-test-ledger-cap");
    let err = adapter
        .submit_turn(&h, TurnInput::UserText("hi".into()))
        .await
        .expect_err("expected budget_exceeded SubmitFailed");

    match err {
        HarnessError::SubmitFailed(msg) => {
            assert!(
                msg.starts_with("budget_exceeded:"),
                "expected `budget_exceeded:` prefix, got: {msg}"
            );
            assert!(
                msg.contains(&format!("{:.4}", DEFAULT_ADVISE_BUDGET_USD_24H)),
                "expected default cap in message, got: {msg}"
            );
        }
        other => panic!("expected SubmitFailed, got {other:?}"),
    }

    std::env::remove_var(CODEX_BIN_ENV);
    std::env::remove_var("CCTEAM_HOME");
}

/// The `append_budget_ledger_row` public helper used by the adapter
/// matches the schema the rest of the cost rollup (advise_vote +
/// doctor --check-cost-orphan) reads — round-trip through disk and
/// verify the 24h sum reflects the new row.
#[test]
fn append_budget_ledger_row_round_trips_through_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    append_budget_ledger_row(root, AgentVendor::Codex, 0.0050).unwrap();
    append_budget_ledger_row(root, AgentVendor::Codex, 0.0050).unwrap();
    let ledger = load_budget_ledger(root).unwrap();
    let codex_rows: Vec<_> = ledger
        .samples
        .iter()
        .filter(|s| matches!(s.vendor, Vendor::Codex))
        .collect();
    assert_eq!(codex_rows.len(), 2);
    let total = sum_advise_today(&ledger);
    assert!(
        (total - 0.0100).abs() < 1e-9,
        "expected 0.0100 total, got {total}"
    );
}

/// Smoke: when `CCTEAM_HOME` is not set, the adapter must NOT panic;
/// it should fall through with the ledger hook silently disabled.
/// Important for early `ccteam doctor`-style probes that may run
/// before the home dir is materialised.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn submit_turn_degrades_silently_when_ccteam_home_unset() {
    std::env::remove_var("CCTEAM_HOME");
    std::env::set_var(CODEX_BIN_ENV, "/bin/true");
    let adapter = CodexExecAdapter::new();
    let h = handle("ccteam-test-ledger-no-home");
    // /bin/true exits 0 immediately → fallback success branch.
    let _ = adapter
        .submit_turn(&h, TurnInput::UserText("hi".into()))
        .await
        .expect("submit_turn must not error when CCTEAM_HOME unset");
    std::env::remove_var(CODEX_BIN_ENV);
}
