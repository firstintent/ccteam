//! V0.4.6 F82 — `workflow.yaml::enabled` + hot-reload + cancel-token tests.
//!
//! These verify:
//! - `WorkflowSpec::enabled` field defaults `true` and round-trips.
//! - `Orchestrator::run_project_with_cancel` short-circuits when
//!   `enabled: false` (writes `workflow_done reason="disabled"`).
//! - `Orchestrator::unroster_project` cancels a running loop +
//!   writes `workflow_done reason="<reason>"` to progress.jsonl.
//! - `Orchestrator::reload_project` cancels + clears `spawned` so a
//!   later spawn cycle can re-roster the slug.
//! - `WorkflowFileWatcher::new` is fail-safe when a project's
//!   workflow.yaml is missing (no error, empty file list).
//!
//! Red-line audit:
//! - No `tmux` / `claude` binary dependency.
//! - No writes outside `tempdir()`.
//! - Cancel path uses `oneshot::Sender::send` → `select` arm in
//!   `run_project_with_cancel`; **no** `JoinHandle::abort()` calls.

#![cfg(feature = "test-util")]

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;
use tempfile::TempDir;

use ccteam_core::orchestrator::{CancelReason, Orchestrator, OrchestratorConfig};
use ccteam_core::workflow::WorkflowSpec;
use ccteam_core::workflow_watcher::WorkflowFileWatcher;
use ccteam_core::CcteamPaths;

// =====================================================================
// Fixture helpers
// =====================================================================

const YAML_DISABLED: &str = "\
name: f82-disabled-test
enabled: false
agents:
  worker:
    executor: claude
    trigger: manual
";

const YAML_ENABLED_DEFAULT: &str = "\
name: f82-default-enabled
agents:
  worker:
    executor: claude
    trigger: manual
";

const YAML_RUNNING: &str = "\
name: f82-running-test
agents:
  fixer:
    executor: claude
    trigger: watch:issues
    parallelism: 1
    input: issues
";

/// Build a temp `(projects_root, ccteam_root)` with a fully bootstrapped
/// slug — state.json + workflow.yaml — so `Orchestrator::run` rosters it.
fn make_project(workflow_yaml: &str) -> (TempDir, TempDir, PathBuf, CcteamPaths, String) {
    let projects_root = tempfile::tempdir().unwrap();
    let ccteam_root = tempfile::tempdir().unwrap();
    let slug = format!(
        "f82-{}",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    );
    let project_dir = projects_root.path().join(&slug);
    std::fs::create_dir_all(project_dir.join(".ccteam")).unwrap();
    std::fs::write(project_dir.join("workflow.yaml"), workflow_yaml).unwrap();
    // Minimal state.json — `Orchestrator::run` needs it via
    // queries::collect_projects.
    let state = ccteam_core::state::ProjectState::initial(slug.clone());
    let state_path = project_dir.join(".ccteam").join("state.json");
    state.save(&state_path).unwrap();

    let paths = CcteamPaths {
        root: ccteam_root.path().to_path_buf(),
        projects_root: projects_root.path().to_path_buf(),
    };
    (projects_root, ccteam_root, project_dir, paths, slug)
}

fn read_events(path: &std::path::Path) -> Vec<Value> {
    if !path.exists() {
        return Vec::new();
    }
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect()
}

// =====================================================================
// t01 — `enabled: false` short-circuits run_project + writes workflow_done
// =====================================================================
#[tokio::test]
async fn t01_enabled_false_blocks_initial_spawn() {
    let (_pr, _cr, _pdir, paths, slug) = make_project(YAML_DISABLED);

    let orch = Orchestrator::new(paths.clone(), OrchestratorConfig::default()).unwrap();
    // Direct invocation — bypasses run() so the test runs in tens of ms.
    orch.run_project(&slug)
        .await
        .expect("run_project must succeed");

    let progress = paths.progress_jsonl(&slug);
    let events = read_events(&progress);
    let done = events
        .iter()
        .find(|e| e.get("event").and_then(Value::as_str) == Some("workflow_done"))
        .expect("workflow_done must be appended");
    assert_eq!(
        done.get("reason").and_then(Value::as_str),
        Some("disabled"),
        "reason must be 'disabled'; events: {:?}",
        events
    );
    // workflow_start MUST NOT be emitted — disabled short-circuit
    // skips initial dispatch entirely.
    assert!(
        !events
            .iter()
            .any(|e| e.get("event").and_then(Value::as_str) == Some("workflow_start")),
        "workflow_start must not appear when enabled=false; events: {:?}",
        events
    );
}

// =====================================================================
// t02 — `enabled` defaults to true (V0.4.5 yaml without the field works)
// =====================================================================
#[tokio::test]
async fn t02_enabled_true_after_false_starts() {
    let (_pr, _cr, _pdir, paths, slug) = make_project(YAML_ENABLED_DEFAULT);

    // Parse: spec must report enabled=true via serde-default.
    let project_dir = paths.project_dir(&slug);
    let spec = WorkflowSpec::load_for_project(&project_dir).unwrap();
    assert!(
        spec.enabled,
        "missing `enabled:` field must serde-default to true"
    );

    // run_project must emit workflow_start (loop entered) — not
    // workflow_done. Use a timeout because the loop parks on the
    // artifact channel.
    let orch = Orchestrator::new(paths.clone(), OrchestratorConfig::default()).unwrap();
    let fut = orch.run_project(&slug);
    let _ = tokio::time::timeout(std::time::Duration::from_millis(300), fut).await;

    let progress = paths.progress_jsonl(&slug);
    let events = read_events(&progress);
    assert!(
        events
            .iter()
            .any(|e| e.get("event").and_then(Value::as_str) == Some("workflow_start")),
        "default-enabled spec must enter loop; events: {:?}",
        events
    );
    assert!(
        !events.iter().any(|e| {
            e.get("event").and_then(Value::as_str) == Some("workflow_done")
                && e.get("reason").and_then(Value::as_str) == Some("disabled")
        }),
        "no spurious workflow_done reason=disabled when enabled=true",
    );
}

// =====================================================================
// t03 — `unroster_project` of a running loop writes workflow_done
// =====================================================================
#[tokio::test]
async fn t03_disable_running_project_clean_exit() {
    let (_pr, _cr, pdir, paths, slug) = make_project(YAML_RUNNING);
    // Pre-create the watch dir so the loop's pre-scan doesn't bail.
    std::fs::create_dir_all(pdir.join("issues")).unwrap();

    let orch = Arc::new(Orchestrator::new(paths.clone(), OrchestratorConfig::default()).unwrap());

    // Start the project loop in a tokio task. The slug must be
    // registered in cancel_handles via the run() startup path. We
    // simulate that here by registering manually + spawning
    // run_project_with_cancel directly so we own the rx half.
    let (tx, rx) = tokio::sync::oneshot::channel::<CancelReason>();
    orch.cancel_handles_test_insert(&slug, tx).await;

    let orch_for_task = Arc::clone(&orch);
    let slug_for_task = slug.clone();
    let task = tokio::spawn(async move {
        orch_for_task
            .run_project_with_cancel(&slug_for_task, Some(rx))
            .await
    });

    // Yield so the task actually starts the event_loop.
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    // Cancel.
    let cancelled = orch.unroster_project(&slug, CancelReason::Disabled).await;
    assert!(cancelled, "unroster_project must return true on first call");

    // Task must end within a couple of polling intervals.
    tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .expect("task must end within 2s")
        .expect("join handle")
        .expect("run_project_with_cancel returns Ok on cancel");

    // Verify progress.jsonl carries the workflow_done event.
    let progress = paths.progress_jsonl(&slug);
    let events = read_events(&progress);
    let done = events
        .iter()
        .find(|e| {
            e.get("event").and_then(Value::as_str) == Some("workflow_done")
                && e.get("reason").and_then(Value::as_str) == Some("disabled")
        })
        .expect("workflow_done reason=disabled must be appended");
    assert_eq!(
        done.get("slug").and_then(Value::as_str),
        Some(slug.as_str())
    );

    // A second cancel on the same slug is a no-op (returns false).
    let second = orch.unroster_project(&slug, CancelReason::Disabled).await;
    assert!(!second, "second unroster on same slug must return false");
}

// =====================================================================
// t04 — `reload_project` cancels + clears spawned for re-roster
// =====================================================================
#[tokio::test]
async fn t04_trigger_change_reload() {
    let (_pr, _cr, pdir, paths, slug) = make_project(YAML_RUNNING);
    std::fs::create_dir_all(pdir.join("issues")).unwrap();

    let orch = Arc::new(Orchestrator::new(paths.clone(), OrchestratorConfig::default()).unwrap());

    // Register the slug as "rostered" so reload_project sees it.
    let (tx, rx) = tokio::sync::oneshot::channel::<CancelReason>();
    orch.cancel_handles_test_insert(&slug, tx).await;
    orch.test_mark_spawned(&slug).await;

    let orch_for_task = Arc::clone(&orch);
    let slug_for_task = slug.clone();
    let task = tokio::spawn(async move {
        orch_for_task
            .run_project_with_cancel(&slug_for_task, Some(rx))
            .await
    });

    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    assert!(orch.is_slug_rostered(&slug).await);
    let did_cancel = orch.reload_project(&slug).await;
    assert!(did_cancel, "reload must cancel a live loop");

    // The slug must be cleared from `spawned` so a future
    // spawn_new_rostered_projects re-rosters it.
    assert!(
        !orch.test_is_spawned(&slug).await,
        "reload_project must clear `spawned` so the slug can be re-spawned"
    );
    // cancel_handles entry consumed.
    assert!(!orch.is_slug_rostered(&slug).await);

    tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .expect("loop must end")
        .expect("join")
        .expect("Ok on cancel");

    // progress.jsonl carries workflow_done with reason=reloaded.
    let progress = paths.progress_jsonl(&slug);
    let events = read_events(&progress);
    assert!(
        events.iter().any(|e| {
            e.get("event").and_then(Value::as_str) == Some("workflow_done")
                && e.get("reason").and_then(Value::as_str) == Some("reloaded")
        }),
        "workflow_done reason=reloaded must be appended; got: {:?}",
        events
    );
}

// =====================================================================
// t05 — Fail-safe: YAML syntax error must not abort the watcher build
// =====================================================================
#[tokio::test]
async fn t05_yaml_syntax_error_fail_safe() {
    // Build a tempdir with a project that has a broken workflow.yaml.
    let tmp = tempfile::tempdir().unwrap();
    let pdir = tmp.path().join("borked");
    std::fs::create_dir_all(&pdir).unwrap();
    std::fs::write(pdir.join("workflow.yaml"), ":::not yaml:::").unwrap();

    // `WorkflowFileWatcher::new` watches the file regardless of parse
    // status — we never parse here, the orchestrator does.
    let projects = vec![("borked".to_string(), pdir.clone())];
    let result = WorkflowFileWatcher::new(&projects);
    assert!(
        result.is_ok(),
        "watcher must build even when workflow.yaml is unparseable: {:?}",
        result.as_ref().err()
    );

    // Loading the broken spec must fail with a parse error — proving
    // the orchestrator's reload path has a clear surface to decide
    // whether to swap in the new spec (it must NOT).
    let load_err = WorkflowSpec::load(&pdir.join("workflow.yaml")).expect_err("parse must fail");
    let msg = format!("{}", load_err);
    assert!(
        msg.contains("parse") || msg.contains("yaml") || msg.contains("YAML"),
        "expected parse-level error, got: {}",
        msg
    );

    // And missing workflow.yaml is also OK — no file to watch.
    let empty_tmp = tempfile::tempdir().unwrap();
    let empty_pdir = empty_tmp.path().join("missing");
    std::fs::create_dir_all(&empty_pdir).unwrap();
    let nothing = vec![("missing".to_string(), empty_pdir)];
    let (_w, _rx) =
        WorkflowFileWatcher::new(&nothing).expect("missing workflow.yaml must not error");

    // Also: a watcher with both nested + root present must register
    // both watches without complaint.
    let dual_tmp = tempfile::tempdir().unwrap();
    let dual_pdir = dual_tmp.path().join("dual");
    std::fs::create_dir_all(dual_pdir.join(".ccteam")).unwrap();
    std::fs::write(
        dual_pdir.join("workflow.yaml"),
        "name: x\nagents:\n  a:\n    trigger: manual\n",
    )
    .unwrap();
    std::fs::write(
        dual_pdir.join(".ccteam").join("workflow.yaml"),
        "name: x\nagents:\n  a:\n    trigger: manual\n",
    )
    .unwrap();
    let dual = vec![("dual".to_string(), dual_pdir)];
    let (_w2, _rx2) =
        WorkflowFileWatcher::new(&dual).expect("dual-location project must register both");
}

// =====================================================================
// Test-only helpers (private to this test crate) — the orchestrator
// exposes register-time hooks under `#[cfg(test, feature="test-util")]`
// so the production surface stays clean.
// =====================================================================

trait OrchestratorTestExt {
    async fn cancel_handles_test_insert(
        &self,
        slug: &str,
        tx: tokio::sync::oneshot::Sender<CancelReason>,
    );
    async fn test_mark_spawned(&self, slug: &str);
    async fn test_is_spawned(&self, slug: &str) -> bool;
}

impl OrchestratorTestExt for Orchestrator {
    async fn cancel_handles_test_insert(
        &self,
        slug: &str,
        tx: tokio::sync::oneshot::Sender<CancelReason>,
    ) {
        self.test_cancel_handles_insert(slug, tx).await
    }
    async fn test_mark_spawned(&self, slug: &str) {
        self.test_spawned_insert(slug).await
    }
    async fn test_is_spawned(&self, slug: &str) -> bool {
        self.test_spawned_contains(slug).await
    }
}
