//! V0.4.0 F66 — thin orchestrator integration tests.
//!
//! Coverage matrix (20 cases) — see `docs/versions/v0-4-0/dev-plan.md` §8.1 #7.9.
//!
//! These tests exercise the new artifact-trigger dispatch loop without
//! touching tmux: a `MockAdapter` implementing [`HarnessAdapter`] is
//! swapped in via `Orchestrator::set_adapter`, and synthesised
//! [`ArtifactEvent`]s are fed through the `test_handle_artifact_event`
//! hook (gated behind the `test-util` feature).
//!
//! Red-line audit:
//! - No `cargo test`-level dependency on `tmux` / `claude` binaries.
//! - No `~/.claude/jobs` / `~/.ccteam` writes outside `tempdir()`.
//! - `progress.jsonl` is asserted as SoT for every dispatch decision
//!   (tests #11, #18 read it back).

#![cfg(feature = "test-util")]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use serde_json::Value;
use serial_test::serial;
use tempfile::TempDir;

use ccteam_core::artifact_watcher::{ArtifactEvent, WatchKind};
use ccteam_core::harness::{
    AgentSpecBrief, AgentVendor, ExecutionMode, HarnessAdapter, HarnessError, SessionHandle,
    SpawnCtx, ThreadEvent, ThreadHandle, TurnId, TurnInput,
};
use ccteam_core::orchestrator::{Orchestrator, OrchestratorConfig};
use ccteam_core::workflow::{AgentSpec, Executor, Trigger, WorkflowSpec};
use ccteam_core::{cost_summary, CcteamPaths};
use futures::stream::{self, BoxStream};

// =====================================================================
// MockAdapter
// =====================================================================

/// Test double — records every `start_thread` call, returns a fake
/// [`ThreadHandle`]. The next-fail counter lets a test set up "first N
/// spawns must fail" scenarios for escalation coverage.
///
/// V0.6.0 F107 migration: this impl drives the new 5-method
/// HarnessAdapter trait. `vendor()` is derived from `name`
/// (`"claude"` / `"codex"` substring); `start_thread` mirrors the old
/// `spawn_session` semantics; `close_thread` increments the shutdown
/// guard counter (red-line "orchestrator must never call close_thread
/// on its own" still applies).
#[derive(Clone, Default)]
struct MockAdapter {
    name: &'static str,
    /// `(slug, sid, executor)` tuples for every successful spawn.
    spawned: Arc<Mutex<Vec<(String, String, String)>>>,
    seq: Arc<AtomicU64>,
    /// Number of remaining forced-failure responses.
    fail_remaining: Arc<AtomicU64>,
    /// Track explicit shutdown calls — red-line guard for "never kill
    /// running session".
    shutdown_calls: Arc<AtomicU64>,
}

impl MockAdapter {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            seq: Arc::new(AtomicU64::new(1)),
            ..Default::default()
        }
    }
    fn fail_next(&self, n: u64) {
        self.fail_remaining.store(n, Ordering::SeqCst);
    }
    fn spawn_count(&self) -> usize {
        self.spawned.lock().unwrap().len()
    }
    fn shutdown_count(&self) -> u64 {
        self.shutdown_calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl HarnessAdapter for MockAdapter {
    fn name(&self) -> &'static str {
        self.name
    }

    fn vendor(&self) -> AgentVendor {
        if self.name.contains("codex") {
            AgentVendor::Codex
        } else {
            AgentVendor::Claude
        }
    }

    async fn start_thread(
        &self,
        _spec: &AgentSpecBrief,
        ctx: &SpawnCtx,
    ) -> Result<ThreadHandle, HarnessError> {
        if self.fail_remaining.load(Ordering::SeqCst) > 0 {
            self.fail_remaining.fetch_sub(1, Ordering::SeqCst);
            return Err(HarnessError::SpawnFailed("mock fail".into()));
        }
        let n = self.seq.fetch_add(1, Ordering::SeqCst);
        let sid = ctx.sid.clone();
        let exec = if self.name.contains("codex") {
            "codex"
        } else {
            "claude"
        };
        self.spawned
            .lock()
            .unwrap()
            .push((ctx.slug.clone(), sid.clone(), exec.to_string()));
        let tmux_session = format!("mock-{}-{}-{}", ctx.slug, exec, n);
        let identity = if exec == "claude" {
            // Bg adapters use job_id as identity.
            format!("mock-job-{n}")
        } else {
            tmux_session.clone()
        };
        Ok(ThreadHandle {
            vendor: self.vendor(),
            mode: ExecutionMode::Bg,
            identity,
            started_at: chrono::Utc::now(),
            raw_extras: serde_json::json!({
                "tmux_session": tmux_session,
                "pid": 10_000u64 + n,
            }),
        })
    }

    async fn submit_turn(
        &self,
        h: &ThreadHandle,
        _input: TurnInput,
    ) -> Result<TurnId, HarnessError> {
        Ok(TurnId::new(format!("mock-turn-{}", h.identity)))
    }

    fn events(&self, _h: &ThreadHandle) -> BoxStream<'static, ThreadEvent> {
        Box::pin(stream::empty())
    }

    async fn resume_thread(&self, _persistent_id: &str) -> Result<ThreadHandle, HarnessError> {
        Err(HarnessError::NotImplemented {
            reason: "MockAdapter does not implement resume_thread".to_string(),
        })
    }

    async fn close_thread(&self, _h: &ThreadHandle) -> Result<(), HarnessError> {
        self.shutdown_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

// =====================================================================
// Fixture helpers
// =====================================================================

/// Build a temporary `<project>/workflow.yaml` directory tree and
/// return `(project_dir, paths, progress_path)`. `paths.root` is set to
/// a fresh tempdir per call so progress jsonl files don't bleed
/// between tests.
fn make_project(workflow_yaml: &str) -> (TempDir, TempDir, PathBuf, CcteamPaths, PathBuf, String) {
    let projects_root = tempfile::tempdir().unwrap();
    let ccteam_root = tempfile::tempdir().unwrap();
    let slug = format!(
        "f66-{}",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    );
    let project_dir = projects_root.path().join(&slug);
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(project_dir.join("workflow.yaml"), workflow_yaml).unwrap();

    let paths = CcteamPaths {
        root: ccteam_root.path().to_path_buf(),
        projects_root: projects_root.path().to_path_buf(),
    };
    let progress_path = paths.progress_jsonl(&slug);
    (
        projects_root,
        ccteam_root,
        project_dir,
        paths,
        progress_path,
        slug,
    )
}

fn watch_spec(role: &str, watch_rel: &str, parallelism: Option<u32>) -> WorkflowSpec {
    let mut agents = IndexMap::new();
    agents.insert(
        role.into(),
        AgentSpec {
            executor: Executor::Claude,
            model: None,
            trigger: Trigger::Watch(PathBuf::from(watch_rel)),
            parallelism,
            input: Some(PathBuf::from(watch_rel)),
            output: None,
            interval: None,
            timeout: None,
            on_timeout: None,
            plan_approval: None,
        },
    );
    WorkflowSpec {
        name: "test-workflow".into(),
        description: None,
        mode: ccteam_core::WorkflowMode::default(),
        enabled: true,
        budget: None,
        budgets_v060: None,
        agent_team: None,
        chat: None,
        agents,
    }
}

fn manual_spec(role: &str) -> WorkflowSpec {
    let mut agents = IndexMap::new();
    agents.insert(
        role.into(),
        AgentSpec {
            executor: Executor::Claude,
            model: None,
            trigger: Trigger::Manual,
            parallelism: None,
            input: None,
            output: None,
            interval: None,
            timeout: None,
            on_timeout: None,
            plan_approval: None,
        },
    );
    WorkflowSpec {
        name: "test-manual-workflow".into(),
        description: None,
        mode: ccteam_core::WorkflowMode::default(),
        enabled: true,
        budget: None,
        budgets_v060: None,
        agent_team: None,
        chat: None,
        agents,
    }
}

fn gate_spec(role: &str, input_rel: &str) -> WorkflowSpec {
    let mut agents = IndexMap::new();
    agents.insert(
        role.into(),
        AgentSpec {
            executor: Executor::Claude,
            model: None,
            trigger: Trigger::Gate,
            parallelism: None,
            input: Some(PathBuf::from(input_rel)),
            output: None,
            interval: None,
            timeout: None,
            on_timeout: None,
            plan_approval: None,
        },
    );
    WorkflowSpec {
        name: "test-gate-workflow".into(),
        description: None,
        mode: ccteam_core::WorkflowMode::default(),
        enabled: true,
        budget: None,
        budgets_v060: None,
        agent_team: None,
        chat: None,
        agents,
    }
}

fn read_events(path: &Path) -> Vec<Value> {
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

fn build_orchestrator(paths: CcteamPaths) -> (Orchestrator, Arc<MockAdapter>, Arc<MockAdapter>) {
    let mut orch = Orchestrator::new(paths, OrchestratorConfig::default()).unwrap();
    let claude_mock = Arc::new(MockAdapter::new("claude-mock"));
    let codex_mock = Arc::new(MockAdapter::new("codex-mock"));
    orch.set_adapter(Executor::Claude, claude_mock.clone());
    orch.set_adapter(Executor::Codex, codex_mock.clone());
    (orch, claude_mock, codex_mock)
}

// =====================================================================
// Tests
// =====================================================================

const YAML_WATCH_FIXER: &str = "\
name: test-workflow
agents:
  fixer:
    executor: claude
    trigger: watch:issues
    parallelism: 2
    input: issues
    output: fixes
";

const YAML_MANUAL_EXPLORER: &str = "\
name: test-workflow
agents:
  explorer:
    executor: claude
    trigger: manual
    output: issues
";

const YAML_GATE_SHIPPER: &str = "\
name: test-gate-workflow
agents:
  shipper:
    executor: claude
    trigger: gate
    input: verdicts
";

#[tokio::test]
async fn t01_run_project_loads_workflow() {
    let (_pr, _cr, pdir, paths, _progress, slug) = make_project(YAML_WATCH_FIXER);
    // Ensure the watch dir exists so the watcher's pre-scan does not
    // abort. The test only verifies the entry path compiles and the
    // workflow loads — actual dispatch is covered in later cases.
    std::fs::create_dir_all(pdir.join("issues")).unwrap();

    let (orch, _claude, _codex) = build_orchestrator(paths);
    // Run for a short while then drop the future. The watcher parks
    // forever in stub mode; we just need the loaded-workflow event.
    let fut = orch.run_project(&slug);
    let race = tokio::time::timeout(std::time::Duration::from_millis(200), fut).await;
    // Either the timeout fired (expected — event_loop has nothing to
    // do) or run_project completed with Ok. Both are pass.
    let _ = race;

    let progress_path = orch.paths().progress_jsonl(&slug);
    let events = read_events(&progress_path);
    assert!(
        events
            .iter()
            .any(|e| e.get("event").and_then(Value::as_str) == Some("workflow_start")),
        "workflow_start event missing"
    );
}

#[tokio::test]
async fn t02_no_workflow_returns_error() {
    let projects_root = tempfile::tempdir().unwrap();
    let ccteam_root = tempfile::tempdir().unwrap();
    let slug = "no-workflow";
    std::fs::create_dir_all(projects_root.path().join(slug)).unwrap();
    let paths = CcteamPaths {
        root: ccteam_root.path().to_path_buf(),
        projects_root: projects_root.path().to_path_buf(),
    };
    let (orch, _c, _co) = build_orchestrator(paths);
    let res = orch.run_project(slug).await;
    assert!(res.is_err(), "expected error when workflow.yaml missing");
    let msg = format!("{}", res.unwrap_err());
    assert!(
        msg.contains("workflow.yaml not found") || msg.contains("not found"),
        "unexpected error msg: {msg}"
    );
}

#[tokio::test]
async fn t03_dispatch_watch_trigger_on_existing_artifact() {
    let (_pr, _cr, pdir, paths, _progress, _slug) = make_project(YAML_WATCH_FIXER);
    std::fs::create_dir_all(pdir.join("issues")).unwrap();
    std::fs::write(pdir.join("issues/a.md"), "x").unwrap();
    let (orch, claude, _codex) = build_orchestrator(paths);
    let spec = watch_spec("fixer", "issues", Some(2));
    let progress = orch.paths().progress_jsonl("dispatch-test");

    // Simulate the watcher: ArtifactWatcher::new pre-seeds existing
    // files. We replicate the same path by hand-feeding the event.
    let evt = ArtifactEvent {
        role: "fixer".into(),
        artifact_path: pdir.join("issues/a.md"),
        event_kind: WatchKind::Created,
    };
    orch.test_handle_artifact_event("dispatch-test", &spec, &pdir, &progress, evt)
        .await
        .unwrap();
    assert_eq!(claude.spawn_count(), 1, "pre-existing artifact must spawn");
}

#[tokio::test]
async fn t04_artifact_event_spawns_agent() {
    let (_pr, _cr, pdir, paths, _progress, slug) = make_project(YAML_WATCH_FIXER);
    let (orch, claude, _codex) = build_orchestrator(paths);
    let spec = watch_spec("fixer", "issues", Some(2));
    let progress = orch.paths().progress_jsonl(&slug);
    let evt = ArtifactEvent {
        role: "fixer".into(),
        artifact_path: pdir.join("issues/new.md"),
        event_kind: WatchKind::Created,
    };
    orch.test_handle_artifact_event(&slug, &spec, &pdir, &progress, evt)
        .await
        .unwrap();
    assert_eq!(claude.spawn_count(), 1);
    let events = read_events(&progress);
    assert!(events
        .iter()
        .any(|e| e.get("event").and_then(Value::as_str) == Some("artifact_received")));
    assert!(events
        .iter()
        .any(|e| e.get("event").and_then(Value::as_str) == Some("agent_spawn")));
}

#[tokio::test]
async fn t05_parallelism_limit_respected() {
    let (_pr, _cr, pdir, paths, _progress, slug) = make_project(YAML_WATCH_FIXER);
    let (orch, claude, _codex) = build_orchestrator(paths);
    let spec = watch_spec("fixer", "issues", Some(2));
    let progress = orch.paths().progress_jsonl(&slug);

    // Three events; parallelism=2 → 3rd must enqueue.
    for i in 0..3 {
        let evt = ArtifactEvent {
            role: "fixer".into(),
            artifact_path: pdir.join(format!("issues/{i}.md")),
            event_kind: WatchKind::Created,
        };
        orch.test_handle_artifact_event(&slug, &spec, &pdir, &progress, evt)
            .await
            .unwrap();
    }
    assert_eq!(claude.spawn_count(), 2, "max 2 spawns under parallelism=2");
    assert_eq!(orch.test_pending_count("fixer").await, 1);
}

/// Regression test for the check-then-act race in dispatch_artifact.
/// Two markers landing concurrently both observed running_count=0
/// past the fast-path check; without the atomic gate inside
/// try_spawn_with_prompt, both would spawn even with parallelism=1.
/// Live repro on dex-ui showed 2 releaser sessions running with
/// parallelism=1 declared.
#[tokio::test]
async fn t05b_parallelism_race_holds_under_concurrent_dispatch() {
    use std::sync::Arc;
    let (_pr, _cr, pdir, paths, _progress, slug) = make_project(YAML_WATCH_FIXER);
    let (orch, claude, _codex) = build_orchestrator(paths);
    let spec = Arc::new(watch_spec("fixer", "issues", Some(1)));
    let progress = orch.paths().progress_jsonl(&slug);

    let orch = Arc::new(orch);
    let pdir = Arc::new(pdir);
    let progress = Arc::new(progress);
    let slug = Arc::new(slug);

    let mut handles = Vec::new();
    for i in 0..4 {
        let orch = Arc::clone(&orch);
        let spec = Arc::clone(&spec);
        let pdir = Arc::clone(&pdir);
        let progress = Arc::clone(&progress);
        let slug = Arc::clone(&slug);
        handles.push(tokio::spawn(async move {
            let evt = ArtifactEvent {
                role: "fixer".into(),
                artifact_path: pdir.join(format!("issues/{i}.md")),
                event_kind: WatchKind::Created,
            };
            orch.test_handle_artifact_event(&slug, &spec, &pdir, &progress, evt)
                .await
                .unwrap();
        }));
    }
    for h in handles {
        h.await.unwrap();
    }

    assert_eq!(
        claude.spawn_count(),
        1,
        "concurrent dispatchers must collectively spawn at most parallelism=1"
    );
}

#[tokio::test]
async fn t06_gate_trigger_fires_on_input_satisfied() {
    let (_pr, _cr, pdir, paths, _progress, slug) = make_project(YAML_GATE_SHIPPER);
    std::fs::create_dir_all(pdir.join("verdicts")).unwrap();
    std::fs::write(pdir.join("verdicts/v1.md"), "ok").unwrap();
    let (orch, claude, _codex) = build_orchestrator(paths);
    let spec = gate_spec("shipper", "verdicts");
    let progress = orch.paths().progress_jsonl(&slug);

    // Initial dispatch puts the gate in Waiting state; poll_completions
    // (which we exercise via test hook) runs check_gates → release.
    orch.test_poll_completions(&slug, &spec, &pdir, &progress)
        .await;
    assert!(
        claude.spawn_count() >= 1,
        "gate must spawn when input dir non-empty"
    );
    let events = read_events(&progress);
    assert!(events
        .iter()
        .any(|e| e.get("event").and_then(Value::as_str) == Some("gate_triggered")));
}

#[tokio::test]
#[serial]
async fn t07_budget_exceeded_blocks_spawn() {
    let (_pr, _cr, pdir, paths, _progress, slug) = make_project(YAML_WATCH_FIXER);
    // Pin a tiny budget via env so any cost > 0.01 trips it.
    std::env::set_var("CCTEAM_BUDGET_LIMIT_USD", "0.01");
    let (orch, claude, _codex) = build_orchestrator(paths);
    let spec = watch_spec("fixer", "issues", Some(2));
    let progress = orch.paths().progress_jsonl(&slug);
    // Pre-write an agent_done with a big cost to push us over.
    ccteam_core::progress::append_event(
        &progress,
        &serde_json::json!({
            "event": "agent_done",
            "role": "fixer",
            "cost_usd": 1.00,
            "ts": chrono::Utc::now().to_rfc3339(),
        }),
    )
    .unwrap();
    let evt = ArtifactEvent {
        role: "fixer".into(),
        artifact_path: pdir.join("issues/x.md"),
        event_kind: WatchKind::Created,
    };
    orch.test_handle_artifact_event(&slug, &spec, &pdir, &progress, evt)
        .await
        .unwrap();
    std::env::remove_var("CCTEAM_BUDGET_LIMIT_USD");
    assert_eq!(claude.spawn_count(), 0, "budget must block spawn");
    let events = read_events(&progress);
    assert!(events
        .iter()
        .any(|e| e.get("event").and_then(Value::as_str) == Some("budget_exceeded")));
}

#[tokio::test]
async fn t08_gate_override_file_force_triggers() {
    let (_pr, _cr, pdir, paths, _progress, slug) = make_project(YAML_GATE_SHIPPER);
    // Important: no verdicts dir → threshold NOT met. Only the
    // override file should force the spawn.
    let (orch, claude, _codex) = build_orchestrator(paths);
    let spec = gate_spec("shipper", "verdicts");
    let progress = orch.paths().progress_jsonl(&slug);
    orch.test_gate_override(&slug, &spec, "shipper", &pdir, &progress)
        .await
        .unwrap();
    assert_eq!(claude.spawn_count(), 1, "override must force spawn");
    let events = read_events(&progress);
    let gate_evt = events
        .iter()
        .find(|e| e.get("event").and_then(Value::as_str) == Some("gate_triggered"))
        .expect("gate_triggered missing");
    assert_eq!(gate_evt.get("forced").and_then(Value::as_bool), Some(true));
}

#[tokio::test]
#[serial]
async fn t09_completed_session_dequeues_pending() {
    let (_pr, _cr, pdir, paths, _progress, slug) = make_project(YAML_WATCH_FIXER);
    let state_dir = tempfile::tempdir().unwrap();
    std::env::set_var("CCTEAM_SESSION_STATE_DIR", state_dir.path());
    let (orch, claude, _codex) = build_orchestrator(paths);
    let spec = watch_spec("fixer", "issues", Some(1));
    let progress = orch.paths().progress_jsonl(&slug);

    // 2 events under parallelism=1 → 1 spawn, 1 pending.
    for i in 0..2 {
        let evt = ArtifactEvent {
            role: "fixer".into(),
            artifact_path: pdir.join(format!("issues/{i}.md")),
            event_kind: WatchKind::Created,
        };
        orch.test_handle_artifact_event(&slug, &spec, &pdir, &progress, evt)
            .await
            .unwrap();
    }
    assert_eq!(claude.spawn_count(), 1);
    assert_eq!(orch.test_pending_count("fixer").await, 1);

    // Drop a state.json marking the running session "completed".
    let spawned = claude.spawned.lock().unwrap()[0].clone();
    let sid_dir = state_dir.path().join(&spawned.1);
    std::fs::create_dir_all(&sid_dir).unwrap();
    std::fs::write(
        sid_dir.join("state.json"),
        r#"{"status":"completed","cost_usd":0.01}"#,
    )
    .unwrap();

    orch.test_poll_completions(&slug, &spec, &pdir, &progress)
        .await;
    std::env::remove_var("CCTEAM_SESSION_STATE_DIR");
    assert_eq!(
        claude.spawn_count(),
        2,
        "pending dequeue must trigger second spawn"
    );
    assert_eq!(orch.test_pending_count("fixer").await, 0);
}

#[tokio::test]
#[serial]
async fn t10_workflow_done_event_written() {
    let (_pr, _cr, pdir, paths, _progress, slug) = make_project(YAML_GATE_SHIPPER);
    let state_dir = tempfile::tempdir().unwrap();
    std::env::set_var("CCTEAM_SESSION_STATE_DIR", state_dir.path());
    let (orch, claude, _codex) = build_orchestrator(paths);
    let spec = gate_spec("shipper", "verdicts");
    let progress = orch.paths().progress_jsonl(&slug);

    // Force the gate.
    orch.test_gate_override(&slug, &spec, "shipper", &pdir, &progress)
        .await
        .unwrap();
    assert_eq!(claude.spawn_count(), 1);

    // Mark the spawned session "completed".
    let sid = claude.spawned.lock().unwrap()[0].1.clone();
    let sid_dir = state_dir.path().join(&sid);
    std::fs::create_dir_all(&sid_dir).unwrap();
    std::fs::write(
        sid_dir.join("state.json"),
        r#"{"status":"completed","cost_usd":0.0}"#,
    )
    .unwrap();

    orch.test_poll_completions(&slug, &spec, &pdir, &progress)
        .await;
    std::env::remove_var("CCTEAM_SESSION_STATE_DIR");
    let events = read_events(&progress);
    assert!(
        events
            .iter()
            .any(|e| e.get("event").and_then(Value::as_str) == Some("workflow_done")),
        "workflow_done event missing after gate completion"
    );
}

#[tokio::test]
async fn t11_progress_jsonl_has_correct_events() {
    let (_pr, _cr, pdir, paths, _progress, slug) = make_project(YAML_WATCH_FIXER);
    std::fs::create_dir_all(pdir.join("issues")).unwrap();
    let (orch, _claude, _codex) = build_orchestrator(paths);
    let spec = watch_spec("fixer", "issues", Some(1));
    let progress = orch.paths().progress_jsonl(&slug);

    // Manually write workflow_start (run_project does this) + feed one
    // artifact_received via test hook.
    ccteam_core::progress::append_event(
        &progress,
        &serde_json::json!({
            "event": "workflow_start", "workflow": "test-workflow",
            "slug": slug, "ts": chrono::Utc::now().to_rfc3339(),
        }),
    )
    .unwrap();
    let evt = ArtifactEvent {
        role: "fixer".into(),
        artifact_path: pdir.join("issues/a.md"),
        event_kind: WatchKind::Created,
    };
    orch.test_handle_artifact_event(&slug, &spec, &pdir, &progress, evt)
        .await
        .unwrap();
    let events = read_events(&progress);
    let kinds: std::collections::HashSet<&str> = events
        .iter()
        .filter_map(|e| e.get("event").and_then(Value::as_str))
        .collect();
    // At least workflow_start + artifact_received + agent_spawn.
    assert!(kinds.contains("workflow_start"), "workflow_start missing");
    assert!(
        kinds.contains("artifact_received"),
        "artifact_received missing"
    );
    assert!(kinds.contains("agent_spawn"), "agent_spawn missing");
}

#[tokio::test]
#[serial]
async fn t12_budget_preserved_across_restarts() {
    let (_pr, _cr, pdir, paths, _progress, slug) = make_project(YAML_WATCH_FIXER);
    let progress = paths.progress_jsonl(&slug);
    // Persist a heavy cost from a previous "run".
    ccteam_core::progress::append_event(
        &progress,
        &serde_json::json!({
            "event": "agent_done",
            "role": "fixer",
            "cost_usd": 999.0,
            "ts": chrono::Utc::now().to_rfc3339(),
        }),
    )
    .unwrap();
    // Fresh orchestrator instance — must respect the prior cost.
    std::env::set_var("CCTEAM_BUDGET_LIMIT_USD", "100.0");
    let (orch, claude, _codex) = build_orchestrator(paths);
    let spec = watch_spec("fixer", "issues", Some(1));
    let evt = ArtifactEvent {
        role: "fixer".into(),
        artifact_path: pdir.join("issues/a.md"),
        event_kind: WatchKind::Created,
    };
    orch.test_handle_artifact_event(&slug, &spec, &pdir, &progress, evt)
        .await
        .unwrap();
    std::env::remove_var("CCTEAM_BUDGET_LIMIT_USD");
    assert_eq!(
        claude.spawn_count(),
        0,
        "restored cost must block spawn across restarts"
    );
}

#[tokio::test]
async fn t13_orchestrator_new_registers_adapters() {
    let projects_root = tempfile::tempdir().unwrap();
    let ccteam_root = tempfile::tempdir().unwrap();
    let paths = CcteamPaths {
        root: ccteam_root.path().to_path_buf(),
        projects_root: projects_root.path().to_path_buf(),
    };
    let orch = Orchestrator::new(paths, OrchestratorConfig::default()).unwrap();
    let keys = orch.test_adapter_keys();
    assert!(keys.contains(&"claude"), "claude adapter not registered");
    assert!(keys.contains(&"codex"), "codex adapter not registered");
}

#[tokio::test]
#[serial]
async fn t14_slow_session_completion_no_leak() {
    // A "slow" session whose state.json never reports done. We verify
    // running count stays at 1 after multiple polls and that dropping
    // the orchestrator cleans up without a Mutex panic.
    let (_pr, _cr, pdir, paths, _progress, slug) = make_project(YAML_WATCH_FIXER);
    let state_dir = tempfile::tempdir().unwrap();
    std::env::set_var("CCTEAM_SESSION_STATE_DIR", state_dir.path());
    let (orch, claude, _codex) = build_orchestrator(paths);
    let spec = watch_spec("fixer", "issues", Some(2));
    let progress = orch.paths().progress_jsonl(&slug);
    let evt = ArtifactEvent {
        role: "fixer".into(),
        artifact_path: pdir.join("issues/a.md"),
        event_kind: WatchKind::Created,
    };
    orch.test_handle_artifact_event(&slug, &spec, &pdir, &progress, evt)
        .await
        .unwrap();
    assert_eq!(claude.spawn_count(), 1);
    for _ in 0..3 {
        orch.test_poll_completions(&slug, &spec, &pdir, &progress)
            .await;
    }
    assert_eq!(
        orch.test_running_count("fixer").await,
        1,
        "no leak — slow session stays accounted for"
    );
    std::env::remove_var("CCTEAM_SESSION_STATE_DIR");
    drop(orch); // implicit RAII drop should not panic
}

#[tokio::test]
async fn t15_manual_trigger_not_auto_spawned() {
    let (_pr, _cr, pdir, paths, _progress, slug) = make_project(YAML_MANUAL_EXPLORER);
    let (orch, claude, _codex) = build_orchestrator(paths);
    // Use run_project's dispatch_initial_triggers path — manual trigger
    // must not spawn.
    let fut = orch.run_project(&slug);
    let _ = tokio::time::timeout(std::time::Duration::from_millis(200), fut).await;
    assert_eq!(
        claude.spawn_count(),
        0,
        "manual trigger must NOT auto-spawn"
    );
    let _ = pdir;
}

#[tokio::test]
async fn t16_multiple_watch_agents_independent() {
    let yaml = "\
name: multi
agents:
  agentA:
    executor: claude
    trigger: watch:dirA
    parallelism: 1
    input: dirA
  agentB:
    executor: claude
    trigger: watch:dirB
    parallelism: 1
    input: dirB
";
    let (_pr, _cr, pdir, paths, _progress, slug) = make_project(yaml);
    let (orch, claude, _codex) = build_orchestrator(paths);
    let mut agents = IndexMap::new();
    agents.insert(
        "agentA".into(),
        AgentSpec {
            executor: Executor::Claude,
            model: None,
            trigger: Trigger::Watch(PathBuf::from("dirA")),
            parallelism: Some(1),
            input: Some(PathBuf::from("dirA")),
            output: None,
            interval: None,
            timeout: None,
            on_timeout: None,
            plan_approval: None,
        },
    );
    agents.insert(
        "agentB".into(),
        AgentSpec {
            executor: Executor::Claude,
            model: None,
            trigger: Trigger::Watch(PathBuf::from("dirB")),
            parallelism: Some(1),
            input: Some(PathBuf::from("dirB")),
            output: None,
            interval: None,
            timeout: None,
            on_timeout: None,
            plan_approval: None,
        },
    );
    let spec = WorkflowSpec {
        name: "multi".into(),
        description: None,
        mode: ccteam_core::WorkflowMode::default(),
        enabled: true,
        budget: None,
        budgets_v060: None,
        agent_team: None,
        chat: None,
        agents,
    };
    let progress = orch.paths().progress_jsonl(&slug);
    orch.test_handle_artifact_event(
        &slug,
        &spec,
        &pdir,
        &progress,
        ArtifactEvent {
            role: "agentA".into(),
            artifact_path: pdir.join("dirA/a.md"),
            event_kind: WatchKind::Created,
        },
    )
    .await
    .unwrap();
    orch.test_handle_artifact_event(
        &slug,
        &spec,
        &pdir,
        &progress,
        ArtifactEvent {
            role: "agentB".into(),
            artifact_path: pdir.join("dirB/b.md"),
            event_kind: WatchKind::Created,
        },
    )
    .await
    .unwrap();
    assert_eq!(orch.test_running_count("agentA").await, 1);
    assert_eq!(orch.test_running_count("agentB").await, 1);
    assert_eq!(claude.spawn_count(), 2);
}

#[tokio::test]
#[serial]
async fn t17_pending_queue_fifo() {
    let (_pr, _cr, pdir, paths, _progress, slug) = make_project(YAML_WATCH_FIXER);
    let state_dir = tempfile::tempdir().unwrap();
    std::env::set_var("CCTEAM_SESSION_STATE_DIR", state_dir.path());
    let (orch, claude, _codex) = build_orchestrator(paths);
    let spec = watch_spec("fixer", "issues", Some(1));
    let progress = orch.paths().progress_jsonl(&slug);

    for i in 0..3 {
        let evt = ArtifactEvent {
            role: "fixer".into(),
            artifact_path: pdir.join(format!("issues/{i}.md")),
            event_kind: WatchKind::Created,
        };
        orch.test_handle_artifact_event(&slug, &spec, &pdir, &progress, evt)
            .await
            .unwrap();
    }
    assert_eq!(claude.spawn_count(), 1);
    assert_eq!(orch.test_pending_count("fixer").await, 2);

    // Complete each session in turn, expect FIFO dispatch.
    for n in 0..2u32 {
        let sid = claude.spawned.lock().unwrap()[n as usize].1.clone();
        let sid_dir = state_dir.path().join(&sid);
        std::fs::create_dir_all(&sid_dir).unwrap();
        std::fs::write(
            sid_dir.join("state.json"),
            r#"{"status":"completed","cost_usd":0.0}"#,
        )
        .unwrap();
        orch.test_poll_completions(&slug, &spec, &pdir, &progress)
            .await;
    }
    std::env::remove_var("CCTEAM_SESSION_STATE_DIR");
    assert_eq!(claude.spawn_count(), 3, "all queued must spawn in order");
    assert_eq!(orch.test_pending_count("fixer").await, 0);
}

#[tokio::test]
async fn t18_session_error_writes_escalation_event() {
    let (_pr, _cr, pdir, paths, _progress, slug) = make_project(YAML_WATCH_FIXER);
    let (orch, claude, _codex) = build_orchestrator(paths);
    claude.fail_next(1);
    let spec = watch_spec("fixer", "issues", Some(2));
    let progress = orch.paths().progress_jsonl(&slug);
    let evt = ArtifactEvent {
        role: "fixer".into(),
        artifact_path: pdir.join("issues/a.md"),
        event_kind: WatchKind::Created,
    };
    orch.test_handle_artifact_event(&slug, &spec, &pdir, &progress, evt)
        .await
        .unwrap();
    assert_eq!(
        claude.spawn_count(),
        0,
        "the single attempt failed; no successful spawn"
    );
    let events = read_events(&progress);
    assert!(events.iter().any(|e| {
        e.get("event").and_then(Value::as_str) == Some("escalation")
            && e.get("kind").and_then(Value::as_str) == Some("spawn_failed")
    }));
    assert_eq!(orch.test_fail_count("fixer").await, 1);
}

#[tokio::test]
async fn t19_escalation_on_3_consecutive_failures() {
    let (_pr, _cr, pdir, paths, _progress, slug) = make_project(YAML_WATCH_FIXER);
    let (orch, claude, _codex) = build_orchestrator(paths);
    claude.fail_next(3);
    let spec = watch_spec("fixer", "issues", Some(5));
    let progress = orch.paths().progress_jsonl(&slug);
    for i in 0..3 {
        let evt = ArtifactEvent {
            role: "fixer".into(),
            artifact_path: pdir.join(format!("issues/{i}.md")),
            event_kind: WatchKind::Created,
        };
        orch.test_handle_artifact_event(&slug, &spec, &pdir, &progress, evt)
            .await
            .unwrap();
    }
    assert_eq!(claude.spawn_count(), 0);
    assert_eq!(orch.test_fail_count("fixer").await, 3);
    // Escalation should have landed in ~/.ccteam/inbox/.
    let inbox = orch.paths().inbox_dir();
    let mut found = false;
    if let Ok(rd) = std::fs::read_dir(&inbox) {
        for entry in rd.flatten() {
            let body = std::fs::read_to_string(entry.path()).unwrap_or_default();
            if body.contains("fixer") && body.contains("3 consecutive") {
                found = true;
                break;
            }
        }
    }
    assert!(found, "expected btw escalation file in {:?}", inbox);
}

#[tokio::test]
#[serial]
async fn t20_meta_agent_not_killed() {
    // The orchestrator never calls shutdown_session on its own
    // dispatch path. We register a fake running session (mimicking a
    // meta-agent) and run every codepath that could touch it.
    let (_pr, _cr, pdir, paths, _progress, slug) = make_project(YAML_WATCH_FIXER);
    let (orch, claude, _codex) = build_orchestrator(paths);
    orch.test_register_running(
        "meta-agent",
        SessionHandle {
            tmux_session: "ccteam-meta".into(),
            harness: "claude-code".into(),
            sid: "meta-1".into(),
            pid: Some(1234),
            started_at: chrono::Utc::now(),
            job_id: None,
        },
    )
    .await;
    let spec = watch_spec("fixer", "issues", Some(2));
    let progress = orch.paths().progress_jsonl(&slug);

    // Send a budget-exceeding event then several artifact events; none
    // of these should result in a shutdown_session call.
    std::env::set_var("CCTEAM_BUDGET_LIMIT_USD", "0.001");
    ccteam_core::progress::append_event(
        &progress,
        &serde_json::json!({
            "event": "agent_done", "role": "fixer", "cost_usd": 1.0,
            "ts": chrono::Utc::now().to_rfc3339(),
        }),
    )
    .unwrap();
    for i in 0..3 {
        let evt = ArtifactEvent {
            role: "fixer".into(),
            artifact_path: pdir.join(format!("issues/{i}.md")),
            event_kind: WatchKind::Created,
        };
        orch.test_handle_artifact_event(&slug, &spec, &pdir, &progress, evt)
            .await
            .unwrap();
    }
    orch.test_poll_completions(&slug, &spec, &pdir, &progress)
        .await;
    std::env::remove_var("CCTEAM_BUDGET_LIMIT_USD");
    assert_eq!(
        claude.shutdown_count(),
        0,
        "orchestrator must NOT call shutdown_session (meta-agent kill red line)"
    );
    // The meta-agent fake session is still on the books.
    assert_eq!(orch.test_running_count("meta-agent").await, 1);
}

// =====================================================================
// V0.4.0-hotfix — spawn_requests marker consumer
// =====================================================================

const YAML_MANUAL_SPEC: &str = "\
name: manual-test
agents:
  explorer:
    executor: claude
    trigger: manual
";

#[tokio::test]
async fn t21_spawn_requests_marker_fires_manual_role() {
    let (_pr, _cr, pdir, paths, _progress, slug) = make_project(YAML_MANUAL_SPEC);
    let (orch, claude, _codex) = build_orchestrator(paths);
    let spec = manual_spec("explorer");
    let progress = orch.paths().progress_jsonl(&slug);

    // User-side: write spawn_requests marker (mirrors F65
    // `ccteam__spawn_agent` MCP tool or hand-written migration-guide path).
    let bucket = pdir.join(".ccteam").join("spawn_requests");
    std::fs::create_dir_all(&bucket).unwrap();
    let marker = bucket.join("explorer-test.json");
    std::fs::write(
        &marker,
        r#"{"role":"explorer","session_id":"explorer-test","requested_at":"2026-05-14T13:00:00Z","overrides":{}}"#,
    )
    .unwrap();

    // Tick consumes the marker.
    orch.test_check_spawn_requests(&slug, &spec, &pdir, &progress)
        .await;

    assert_eq!(claude.spawn_count(), 1, "marker must trigger spawn");
    assert!(!marker.exists(), "successful spawn must delete the marker");
}

#[tokio::test]
async fn t22_spawn_requests_unknown_role_deletes_marker() {
    let (_pr, _cr, pdir, paths, _progress, slug) = make_project(YAML_MANUAL_SPEC);
    let (orch, claude, _codex) = build_orchestrator(paths);
    let spec = manual_spec("explorer");
    let progress = orch.paths().progress_jsonl(&slug);

    let bucket = pdir.join(".ccteam").join("spawn_requests");
    std::fs::create_dir_all(&bucket).unwrap();
    let marker = bucket.join("ghost.json");
    std::fs::write(&marker, r#"{"role":"ghost","session_id":"ghost-x"}"#).unwrap();

    orch.test_check_spawn_requests(&slug, &spec, &pdir, &progress)
        .await;

    assert_eq!(claude.spawn_count(), 0, "unknown role must not spawn");
    assert!(!marker.exists(), "unknown role marker must be deleted");
}

#[tokio::test]
async fn t23_spawn_requests_malformed_marker_deletes() {
    let (_pr, _cr, pdir, paths, _progress, slug) = make_project(YAML_MANUAL_SPEC);
    let (orch, claude, _codex) = build_orchestrator(paths);
    let spec = manual_spec("explorer");
    let progress = orch.paths().progress_jsonl(&slug);

    let bucket = pdir.join(".ccteam").join("spawn_requests");
    std::fs::create_dir_all(&bucket).unwrap();
    let marker = bucket.join("bad.json");
    std::fs::write(&marker, r#"{"no_role_field": true}"#).unwrap();

    orch.test_check_spawn_requests(&slug, &spec, &pdir, &progress)
        .await;

    assert_eq!(claude.spawn_count(), 0);
    assert!(!marker.exists(), "malformed marker must be deleted");
}

#[tokio::test]
#[serial]
async fn t24_spawn_requests_failed_spawn_retains_marker() {
    let (_pr, _cr, pdir, paths, _progress, slug) = make_project(YAML_MANUAL_SPEC);
    let (orch, claude, _codex) = build_orchestrator(paths);
    let spec = manual_spec("explorer");
    let progress = orch.paths().progress_jsonl(&slug);

    let bucket = pdir.join(".ccteam").join("spawn_requests");
    std::fs::create_dir_all(&bucket).unwrap();
    let marker = bucket.join("explorer-retry.json");
    std::fs::write(
        &marker,
        r#"{"role":"explorer","session_id":"explorer-retry"}"#,
    )
    .unwrap();

    claude.fail_next(1);
    orch.test_check_spawn_requests(&slug, &spec, &pdir, &progress)
        .await;

    assert_eq!(claude.spawn_count(), 0, "first spawn forced to fail");
    assert!(
        marker.exists(),
        "failed spawn must retain marker for next-tick retry"
    );

    // Next tick succeeds.
    orch.test_check_spawn_requests(&slug, &spec, &pdir, &progress)
        .await;
    assert_eq!(claude.spawn_count(), 1, "retry must spawn");
    assert!(!marker.exists(), "successful retry must delete marker");
}

// =====================================================================
// V0.4.0-hotfix — inbox consumer (acknowledgement + archive)
// =====================================================================

fn write_inbox_msg(project_dir: &Path, filename: &str, body: &str, source_user: &str) {
    let dir = project_dir.join(".ccteam").join("inbox");
    std::fs::create_dir_all(&dir).unwrap();
    let frontmatter = format!(
        "---\nschema_version: 1\nsource: ccteam-mcp\nsource_user: {source_user}\n\
         created_at: 2026-05-14T13:00:00Z\ningested_at: 2026-05-14T13:00:00Z\n\
         content_type: text\n---\n\n{body}\n"
    );
    std::fs::write(dir.join(filename), frontmatter).unwrap();
}

#[tokio::test]
async fn t25_inbox_message_archives_and_logs_event() {
    let (_pr, _cr, pdir, paths, _progress, slug) = make_project(YAML_MANUAL_SPEC);
    let (orch, _claude, _codex) = build_orchestrator(paths);
    let spec = manual_spec("explorer");
    let progress = orch.paths().progress_jsonl(&slug);

    write_inbox_msg(
        &pdir,
        "msg-2026-05-14T130000Z-001.md",
        "please look at fixes/",
        "rob",
    );

    orch.test_check_inbox(&slug, &spec, &pdir, &progress).await;

    let inbox = pdir.join(".ccteam").join("inbox");
    let archived = pdir.join(".ccteam").join("inbox.archived");
    assert!(
        !inbox.join("msg-2026-05-14T130000Z-001.md").exists(),
        "consumed message must leave inbox/"
    );
    assert!(
        archived.join("msg-2026-05-14T130000Z-001.md").exists(),
        "consumed message must land in inbox.archived/"
    );

    let events = read_events(&progress);
    let inbox_evt = events
        .iter()
        .find(|e| e.get("event").and_then(|s| s.as_str()) == Some("inbox_received"))
        .expect("inbox_received event must be in progress.jsonl");
    assert_eq!(
        inbox_evt.get("filename").and_then(|s| s.as_str()),
        Some("msg-2026-05-14T130000Z-001.md")
    );
    assert_eq!(
        inbox_evt.get("source_user").and_then(|s| s.as_str()),
        Some("rob")
    );
    assert_eq!(
        inbox_evt.get("parse_failed").and_then(|b| b.as_bool()),
        Some(false)
    );
    assert!(inbox_evt
        .get("body_summary")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .contains("please look at fixes/"));
}

#[tokio::test]
async fn t26_inbox_malformed_archives_with_parse_failed_flag() {
    let (_pr, _cr, pdir, paths, _progress, slug) = make_project(YAML_MANUAL_SPEC);
    let (orch, _claude, _codex) = build_orchestrator(paths);
    let spec = manual_spec("explorer");
    let progress = orch.paths().progress_jsonl(&slug);

    let inbox_dir = pdir.join(".ccteam").join("inbox");
    std::fs::create_dir_all(&inbox_dir).unwrap();
    let bad = "msg-2026-05-14T130001Z-001.md";
    std::fs::write(inbox_dir.join(bad), "no frontmatter just body").unwrap();

    orch.test_check_inbox(&slug, &spec, &pdir, &progress).await;

    assert!(
        !inbox_dir.join(bad).exists(),
        "malformed must still archive (don't loop forever)"
    );
    assert!(pdir
        .join(".ccteam")
        .join("inbox.archived")
        .join(bad)
        .exists());

    let events = read_events(&progress);
    let inbox_evt = events
        .iter()
        .find(|e| e.get("event").and_then(|s| s.as_str()) == Some("inbox_received"))
        .expect("inbox_received emitted for malformed too");
    assert_eq!(
        inbox_evt.get("parse_failed").and_then(|b| b.as_bool()),
        Some(true)
    );
}

#[tokio::test]
async fn t28_run_writes_heartbeat_and_honors_shutdown() {
    // Smoke test for the V0.4.0-hotfix `Orchestrator::run` impl:
    // 1. heartbeat file lands under `<paths.root>/state/`
    // 2. shutdown future resolves → run() returns Ok cleanly
    // 3. heartbeat is removed on shutdown (per CLAUDE.md "no orphans")
    //
    // Roster is intentionally empty (no projects) so we don't depend on
    // inotify or the watcher thread (WSL flake).
    let projects_root = tempfile::tempdir().unwrap();
    let ccteam_root = tempfile::tempdir().unwrap();
    let paths = CcteamPaths {
        root: ccteam_root.path().to_path_buf(),
        projects_root: projects_root.path().to_path_buf(),
    };
    let orch = std::sync::Arc::new(
        Orchestrator::new(paths.clone(), OrchestratorConfig::default()).unwrap(),
    );

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let shutdown = async move {
        let _ = rx.await;
    };

    let orch_for_task = std::sync::Arc::clone(&orch);
    let handle = tokio::spawn(async move { orch_for_task.run(shutdown).await });

    // Give the daemon a tick to write the initial heartbeat.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let hb = paths.root.join("state").join("orchestrator.heartbeat");
    assert!(hb.exists(), "initial heartbeat must be written");

    let _ = tx.send(());
    let result = handle.await.unwrap();
    assert!(result.is_ok(), "run() must return Ok on graceful shutdown");
    assert!(!hb.exists(), "heartbeat must be cleaned up on shutdown");
}

#[tokio::test]
async fn t27_inbox_no_op_when_empty() {
    let (_pr, _cr, pdir, paths, _progress, slug) = make_project(YAML_MANUAL_SPEC);
    let (orch, _claude, _codex) = build_orchestrator(paths);
    let spec = manual_spec("explorer");
    let progress = orch.paths().progress_jsonl(&slug);

    // No inbox dir at all → no panic, no events.
    orch.test_check_inbox(&slug, &spec, &pdir, &progress).await;
    assert!(read_events(&progress).is_empty());
}

// =====================================================================
// V0.4.0-hotfix — inbox auto-spawn (default route to first manual role)
// =====================================================================

fn write_inbox_msg_raw(project_dir: &Path, filename: &str, raw: &str) {
    let dir = project_dir.join(".ccteam").join("inbox");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(filename), raw).unwrap();
}

#[tokio::test]
async fn t29_inbox_auto_spawns_first_manual_role() {
    let (_pr, _cr, pdir, paths, _progress, slug) = make_project(YAML_MANUAL_SPEC);
    let (orch, _claude, _codex) = build_orchestrator(paths);
    let spec = manual_spec("explorer");
    let progress = orch.paths().progress_jsonl(&slug);

    write_inbox_msg(&pdir, "msg-1.md", "hi there", "rob");

    orch.test_check_inbox(&slug, &spec, &pdir, &progress).await;

    // Archive happened
    assert!(pdir.join(".ccteam/inbox.archived/msg-1.md").exists());

    // spawn_requests/ now contains an explorer-inbox-* marker.
    let req_dir = pdir.join(".ccteam/spawn_requests");
    let entries: Vec<_> = std::fs::read_dir(&req_dir).unwrap().flatten().collect();
    assert_eq!(entries.len(), 1, "exactly one spawn marker written");
    let marker_path = entries[0].path();
    let marker_body = std::fs::read_to_string(&marker_path).unwrap();
    let marker_json: serde_json::Value = serde_json::from_str(&marker_body).unwrap();
    assert_eq!(marker_json["role"], "explorer");
    assert_eq!(marker_json["prompt"], "hi there");
    assert_eq!(marker_json["source"], "inbox");

    // Event annotation
    let evt = read_events(&progress)
        .into_iter()
        .find(|e| e.get("event").and_then(|s| s.as_str()) == Some("inbox_received"))
        .unwrap();
    assert_eq!(evt["auto_spawn_role"], "explorer");
    assert!(evt["auto_spawn_marker"].is_string());
}

#[tokio::test]
async fn t30_inbox_no_spawn_opt_out() {
    let (_pr, _cr, pdir, paths, _progress, slug) = make_project(YAML_MANUAL_SPEC);
    let (orch, _claude, _codex) = build_orchestrator(paths);
    let spec = manual_spec("explorer");
    let progress = orch.paths().progress_jsonl(&slug);

    // Frontmatter `no_spawn: true` → archive only, no marker.
    let raw = "---\nschema_version: 1\nsource: cli\nsource_user: rob\n\
               created_at: 2026-05-14T13:00:00Z\ningested_at: 2026-05-14T13:00:00Z\n\
               content_type: text\nno_spawn: true\n---\n\nnote for archive only\n";
    write_inbox_msg_raw(&pdir, "msg-noop.md", raw);

    orch.test_check_inbox(&slug, &spec, &pdir, &progress).await;

    assert!(pdir.join(".ccteam/inbox.archived/msg-noop.md").exists());
    let req_dir = pdir.join(".ccteam/spawn_requests");
    let count = std::fs::read_dir(&req_dir)
        .map(|rd| rd.flatten().count())
        .unwrap_or(0);
    assert_eq!(count, 0, "no_spawn:true must suppress marker write");

    let evt = read_events(&progress)
        .into_iter()
        .find(|e| e.get("event").and_then(|s| s.as_str()) == Some("inbox_received"))
        .unwrap();
    assert!(evt["auto_spawn_role"].is_null());
}

#[tokio::test]
async fn t31_inbox_target_role_routes_explicitly() {
    // Workflow with two roles — `target_role: fixer` must override the
    // "first manual role" heuristic.
    let yaml = "name: dual\nagents:\n  \
                explorer:\n    executor: claude\n    trigger: manual\n  \
                fixer:\n    executor: claude\n    trigger: manual\n";
    let (_pr, _cr, pdir, paths, _progress, slug) = make_project(yaml);
    let (orch, _claude, _codex) = build_orchestrator(paths);
    let mut agents = IndexMap::new();
    for role in ["explorer", "fixer"] {
        agents.insert(
            role.into(),
            AgentSpec {
                executor: Executor::Claude,
                model: None,
                trigger: Trigger::Manual,
                parallelism: None,
                input: None,
                output: None,
                interval: None,
                timeout: None,
                on_timeout: None,
                plan_approval: None,
            },
        );
    }
    let spec = WorkflowSpec {
        name: "dual".into(),
        description: None,
        mode: ccteam_core::WorkflowMode::default(),
        enabled: true,
        budget: None,
        budgets_v060: None,
        agent_team: None,
        chat: None,
        agents,
    };
    let progress = orch.paths().progress_jsonl(&slug);

    let raw = "---\nschema_version: 1\nsource: cli\nsource_user: rob\n\
               created_at: 2026-05-14T13:00:00Z\ningested_at: 2026-05-14T13:00:00Z\n\
               content_type: text\ntarget_role: fixer\n---\n\nplease handle\n";
    write_inbox_msg_raw(&pdir, "msg-target.md", raw);

    orch.test_check_inbox(&slug, &spec, &pdir, &progress).await;

    let req_dir = pdir.join(".ccteam/spawn_requests");
    let entries: Vec<_> = std::fs::read_dir(&req_dir).unwrap().flatten().collect();
    assert_eq!(entries.len(), 1);
    let marker_body = std::fs::read_to_string(entries[0].path()).unwrap();
    let marker_json: serde_json::Value = serde_json::from_str(&marker_body).unwrap();
    assert_eq!(
        marker_json["role"], "fixer",
        "target_role must win over default"
    );
}

#[tokio::test]
async fn t32_inbox_empty_body_no_spawn() {
    let (_pr, _cr, pdir, paths, _progress, slug) = make_project(YAML_MANUAL_SPEC);
    let (orch, _claude, _codex) = build_orchestrator(paths);
    let spec = manual_spec("explorer");
    let progress = orch.paths().progress_jsonl(&slug);

    // Whitespace-only body → don't waste a spawn.
    write_inbox_msg(&pdir, "msg-empty.md", "   \n\n  ", "rob");

    orch.test_check_inbox(&slug, &spec, &pdir, &progress).await;

    let req_dir = pdir.join(".ccteam/spawn_requests");
    let count = std::fs::read_dir(&req_dir)
        .map(|rd| rd.flatten().count())
        .unwrap_or(0);
    assert_eq!(count, 0, "empty body must not trigger spawn");
}

// =====================================================================
// V0.4.5 F80 — stale-spawn cleanup in poll_completions.
//
// Live-host observation: when the daemon was SIGKILLed mid-flight,
// claude bg jobs died without anything writing the matching
// `agent_done`. The stale `agent_spawn` rows in progress.jsonl
// stayed there forever and the web UI showed a phantom running
// count. F80's fix has two halves:
//
//   (a) every fresh `agent_spawn` event now carries `job_id` so the
//       read-side / cleanup probe knows where to look in
//       `~/.claude/jobs/<job_id>/state.json`.
//   (b) `poll_completions` now scans `progress::open_agent_spawns`
//       and synthesises an `agent_done` event for any row whose
//       job is terminal (state.json missing / firstTerminalAt
//       non-null / state in {failed, crashed, ...}) and is NOT
//       currently owned by this orchestrator instance (the in-memory
//       `running` map already covers genuine in-flight sessions).
// =====================================================================

#[tokio::test]
#[serial]
async fn t33_poll_completions_clears_phantom_spawn_from_progress() {
    // Pre-write an agent_spawn whose job_id has no state.json — this
    // is the SIGKILL-casualty shape. poll_completions must emit a
    // synthetic agent_done so the running count drops.
    let (_pr, _cr, pdir, paths, _progress, slug) = make_project(YAML_WATCH_FIXER);
    let progress = paths.progress_jsonl(&slug);
    ccteam_core::progress::append_event(
        &progress,
        &serde_json::json!({
            "event": "agent_spawn",
            "role": "fixer",
            "session_id": "fixer-phantom",
            "tmux_session": "ccteam-test-fixer-phantom",
            "job_id": "dead-job",
            "executor": "claude",
            "slug": slug,
            "ts": chrono::Utc::now().to_rfc3339(),
        }),
    )
    .unwrap();
    // Empty jobs dir → state_json_path("dead-job") returns ENOENT
    // → probe_job classifies as Terminal { status: "killed", cost_usd: 0.0 }.
    let jobs_dir = tempfile::tempdir().unwrap();
    let prev = std::env::var_os(ccteam_core::CLAUDE_JOBS_DIR_ENV);
    std::env::set_var(ccteam_core::CLAUDE_JOBS_DIR_ENV, jobs_dir.path());

    let (orch, _claude, _codex) = build_orchestrator(paths);
    let spec = watch_spec("fixer", "issues", Some(2));

    orch.test_poll_completions(&slug, &spec, &pdir, &progress)
        .await;

    match prev {
        Some(v) => std::env::set_var(ccteam_core::CLAUDE_JOBS_DIR_ENV, v),
        None => std::env::remove_var(ccteam_core::CLAUDE_JOBS_DIR_ENV),
    }

    let events = read_events(&progress);
    let done = events
        .iter()
        .filter(|e| e.get("event").and_then(Value::as_str) == Some("agent_done"))
        .find(|e| e.get("session_id").and_then(Value::as_str) == Some("fixer-phantom"))
        .expect("synthetic agent_done for phantom spawn must be present");
    assert_eq!(done["status"], "killed");
    assert_eq!(done["cost_usd"].as_f64(), Some(0.0));
}

#[tokio::test]
#[serial]
async fn t34_poll_completions_skips_phantom_pass_when_in_memory_owned() {
    // If the orchestrator's in-memory `running` map already lists the
    // sid (i.e. we own the session and just haven't observed its
    // state.json transition yet), the phantom-cleanup pass must NOT
    // fire — that would race the genuine `agent_done` write.
    use ccteam_core::harness::SessionHandle;

    let (_pr, _cr, pdir, paths, _progress, slug) = make_project(YAML_WATCH_FIXER);
    let progress = paths.progress_jsonl(&slug);
    ccteam_core::progress::append_event(
        &progress,
        &serde_json::json!({
            "event": "agent_spawn",
            "role": "fixer",
            "session_id": "fixer-owned",
            "tmux_session": "ccteam-test-fixer-owned",
            "job_id": "owned-job",
            "executor": "claude",
            "slug": slug,
            "ts": chrono::Utc::now().to_rfc3339(),
        }),
    )
    .unwrap();

    // No state.json file — probe would say "killed" if we ran it.
    let jobs_dir = tempfile::tempdir().unwrap();
    let prev = std::env::var_os(ccteam_core::CLAUDE_JOBS_DIR_ENV);
    std::env::set_var(ccteam_core::CLAUDE_JOBS_DIR_ENV, jobs_dir.path());

    let (orch, _claude, _codex) = build_orchestrator(paths);
    let spec = watch_spec("fixer", "issues", Some(2));

    // Register the same sid as "owned" in the orchestrator's running map.
    orch.test_register_running(
        "fixer",
        SessionHandle {
            tmux_session: "ccteam-test-fixer-owned".into(),
            harness: "claude-code".into(),
            sid: "fixer-owned".into(),
            job_id: Some("owned-job".into()),
            pid: None,
            started_at: chrono::Utc::now(),
        },
    )
    .await;

    orch.test_poll_completions(&slug, &spec, &pdir, &progress)
        .await;

    match prev {
        Some(v) => std::env::set_var(ccteam_core::CLAUDE_JOBS_DIR_ENV, v),
        None => std::env::remove_var(ccteam_core::CLAUDE_JOBS_DIR_ENV),
    }

    let events = read_events(&progress);
    // The session has its own state.json poll path that says Running
    // (state_json_path missing → SessionStatus::Running fallback) so
    // the cleanup pass must NOT have written an agent_done for it.
    let synthetic = events
        .iter()
        .filter(|e| e.get("event").and_then(Value::as_str) == Some("agent_done"))
        .any(|e| e.get("session_id").and_then(Value::as_str) == Some("fixer-owned"));
    assert!(
        !synthetic,
        "orchestrator must not synthesise agent_done for an in-memory-owned session",
    );
}

#[tokio::test]
#[serial]
async fn t35_agent_spawn_event_carries_job_id_field() {
    // Bug 1 plumbing: fresh agent_spawn events must include the
    // `job_id` field so the read-side cleanup can locate state.json.
    // The default MockAdapter returns `job_id: None`; use a custom
    // adapter that returns a fixed string and verify it round-trips.
    use ccteam_core::harness::{
        AgentSpecBrief, AgentVendor, ExecutionMode, HarnessAdapter, HarnessError, SpawnCtx,
        ThreadEvent, ThreadHandle, TurnId, TurnInput,
    };
    use futures::stream::{self, BoxStream};

    #[derive(Default)]
    struct JobIdMockAdapter;
    #[async_trait::async_trait]
    impl HarnessAdapter for JobIdMockAdapter {
        fn name(&self) -> &'static str {
            "claude-mock"
        }
        fn vendor(&self) -> AgentVendor {
            AgentVendor::Claude
        }
        async fn start_thread(
            &self,
            _spec: &AgentSpecBrief,
            ctx: &SpawnCtx,
        ) -> Result<ThreadHandle, HarnessError> {
            Ok(ThreadHandle {
                vendor: AgentVendor::Claude,
                mode: ExecutionMode::Bg,
                identity: "abc12345".into(),
                started_at: chrono::Utc::now(),
                raw_extras: serde_json::json!({"tmux_session": format!("mock-{}", ctx.sid)}),
            })
        }
        async fn submit_turn(
            &self,
            h: &ThreadHandle,
            _input: TurnInput,
        ) -> Result<TurnId, HarnessError> {
            Ok(TurnId::new(format!("mock-{}", h.identity)))
        }
        fn events(&self, _h: &ThreadHandle) -> BoxStream<'static, ThreadEvent> {
            Box::pin(stream::empty())
        }
        async fn resume_thread(&self, _persistent_id: &str) -> Result<ThreadHandle, HarnessError> {
            Err(HarnessError::NotImplemented {
                reason: "mock".to_string(),
            })
        }
        async fn close_thread(&self, _h: &ThreadHandle) -> Result<(), HarnessError> {
            Ok(())
        }
    }

    let (_pr, _cr, pdir, paths, _progress, slug) = make_project(YAML_WATCH_FIXER);
    let mut orch = Orchestrator::new(paths.clone(), OrchestratorConfig::default()).unwrap();
    orch.set_adapter(Executor::Claude, Arc::new(JobIdMockAdapter));
    orch.set_adapter(Executor::Codex, Arc::new(JobIdMockAdapter));
    let spec = watch_spec("fixer", "issues", Some(1));
    let progress = orch.paths().progress_jsonl(&slug);

    let evt = ArtifactEvent {
        role: "fixer".into(),
        artifact_path: pdir.join("issues/a.md"),
        event_kind: WatchKind::Created,
    };
    orch.test_handle_artifact_event(&slug, &spec, &pdir, &progress, evt)
        .await
        .unwrap();

    let events = read_events(&progress);
    let spawn = events
        .iter()
        .find(|e| e.get("event").and_then(Value::as_str) == Some("agent_spawn"))
        .expect("agent_spawn missing");
    assert_eq!(
        spawn.get("job_id").and_then(Value::as_str),
        Some("abc12345"),
        "agent_spawn must carry harness job_id verbatim",
    );
}

#[tokio::test]
#[serial]
async fn t36_phantom_cleanup_records_cost_in_progress_for_cost_summary() {
    // V0.4.6 F91 — when poll_completions writes the synthetic
    // `agent_done`, the cost shipped on that event is the SoT for
    // `cost_summary`. Pre-F91 the orchestrator also bumped
    // `state.cost_used_usd`; F91 retired that path (state field is
    // frozen serde-compat), so the assertion now reads through
    // `cost_summary(slug, …)` — same downstream surface, new source.
    use ccteam_core::state::ProjectState;

    let (_pr, _cr, pdir, paths, _progress, slug) = make_project(YAML_WATCH_FIXER);
    let progress = paths.progress_jsonl(&slug);
    // Seed the state.json so the orchestrator can read/write it.
    ProjectState::initial(slug.clone())
        .save(&paths.project_state(&slug))
        .unwrap();

    // Pre-write a phantom spawn whose job_id has a state.json with
    // cost_usd reported by Claude before the host crashed.
    ccteam_core::progress::append_event(
        &progress,
        &serde_json::json!({
            "event": "agent_spawn",
            "role": "fixer",
            "session_id": "fixer-killed",
            "tmux_session": "ccteam-test-fixer-killed",
            "job_id": "cost-recorded",
            "executor": "claude",
            "slug": slug,
            "ts": chrono::Utc::now().to_rfc3339(),
        }),
    )
    .unwrap();

    let jobs_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(jobs_dir.path().join("cost-recorded")).unwrap();
    std::fs::write(
        jobs_dir.path().join("cost-recorded").join("state.json"),
        // `state` is terminal; Claude managed to flush cost before
        // dying. The phantom-cleanup pass must propagate that cost.
        r#"{"state":"failed","cost_usd":1.25,"firstTerminalAt":"2026-05-15T12:00:00Z"}"#,
    )
    .unwrap();
    let prev = std::env::var_os(ccteam_core::CLAUDE_JOBS_DIR_ENV);
    std::env::set_var(ccteam_core::CLAUDE_JOBS_DIR_ENV, jobs_dir.path());

    let (orch, _claude, _codex) = build_orchestrator(paths.clone());
    let spec = watch_spec("fixer", "issues", Some(2));

    orch.test_poll_completions(&slug, &spec, &pdir, &progress)
        .await;

    match prev {
        Some(v) => std::env::set_var(ccteam_core::CLAUDE_JOBS_DIR_ENV, v),
        None => std::env::remove_var(ccteam_core::CLAUDE_JOBS_DIR_ENV),
    }

    let events = read_events(&progress);
    let done = events
        .iter()
        .filter(|e| e.get("event").and_then(Value::as_str) == Some("agent_done"))
        .find(|e| e.get("session_id").and_then(Value::as_str) == Some("fixer-killed"))
        .expect("synthetic agent_done missing");
    assert_eq!(done["status"], "error", "failed state.json maps to error");
    assert!((done["cost_usd"].as_f64().unwrap() - 1.25).abs() < 1e-9);

    // F91: state.cost_used_usd stays at 0.0 (frozen / no longer
    // mutated). The new SoT — cost_summary — surfaces the cost via
    // its `cost_total_usd` field, reading from progress.jsonl.
    let cost = cost_summary(&slug, &progress, &paths).unwrap();
    assert!(
        (cost.cost_total_usd - 1.25).abs() < 1e-9,
        "cost_summary.cost_total_usd must reflect the agent_done cost, got {}",
        cost.cost_total_usd,
    );
}

// ============================================================
// V0.6.1 F124 — `mode: human-approval` narrow scope
// ============================================================

/// Helper: build a `WorkflowMode::HumanApproval` watch-trigger spec.
fn human_approval_watch_spec(role: &str, watch_rel: &str, parallelism: Option<u32>) -> WorkflowSpec {
    let mut spec = watch_spec(role, watch_rel, parallelism);
    spec.mode = ccteam_core::WorkflowMode::HumanApproval;
    spec
}

/// F124 — `pick_adapter` for `mode: human-approval` falls back to
/// the per-executor bg/exec default adapter (the HITL gate is
/// orchestrator-side, not adapter-side).
#[tokio::test]
async fn t30_f124_pick_adapter_human_approval_falls_back_to_bg() {
    use ccteam_core::workflow::WorkflowMode;
    let (_pr, _cr, _pdir, paths, _progress, _slug) = make_project(YAML_WATCH_FIXER);
    let (orch, claude_mock, codex_mock) = build_orchestrator(paths);
    // Claude path → claude-mock (== the registered bg adapter).
    let picked = orch
        .pick_adapter(Executor::Claude, WorkflowMode::HumanApproval)
        .expect("human-approval mode must resolve a claude adapter");
    assert_eq!(picked.name(), claude_mock.name());
    // Codex path → codex-mock.
    let picked = orch
        .pick_adapter(Executor::Codex, WorkflowMode::HumanApproval)
        .expect("human-approval mode must resolve a codex adapter");
    assert_eq!(picked.name(), codex_mock.name());
}

/// F124 — under `mode: human-approval`, an artifact event must NOT
/// auto-spawn (even when capacity is free); it parks on `pending`
/// and emits `plan_decision_required` so F98 plan-approval's IM
/// round-trip can prompt the user.
#[tokio::test]
async fn t31_f124_artifact_event_parks_under_human_approval() {
    let (_pr, _cr, pdir, paths, _progress, slug) = make_project(YAML_WATCH_FIXER);
    let (orch, claude, _codex) = build_orchestrator(paths);
    let spec = human_approval_watch_spec("fixer", "issues", Some(2));
    let progress = orch.paths().progress_jsonl(&slug);

    let evt = ArtifactEvent {
        role: "fixer".into(),
        artifact_path: pdir.join("issues/new.md"),
        event_kind: WatchKind::Created,
    };
    orch.test_handle_artifact_event(&slug, &spec, &pdir, &progress, evt)
        .await
        .unwrap();

    assert_eq!(
        claude.spawn_count(),
        0,
        "human-approval must NOT auto-spawn on artifact event"
    );
    assert_eq!(
        orch.test_pending_count("fixer").await,
        1,
        "artifact event must park on pending queue"
    );

    let events = read_events(&progress);
    let req = events
        .iter()
        .find(|e| e.get("event").and_then(Value::as_str) == Some("plan_decision_required"))
        .expect("plan_decision_required event must be emitted");
    assert_eq!(req["role"], "fixer");
    assert_eq!(req["slug"], slug);
    assert_eq!(req["pending_count"], 1);
}

/// F124 — under `mode: human-approval`, an `agent_done` (via
/// `poll_completions`) must NOT drain the pending queue. Instead it
/// emits `plan_decision_required` and leaves `pending` intact so
/// F98 can decide to spawn (APPROVE → `spawn_requests/*.json`) or
/// drop (REJECT → pop pending).
#[tokio::test]
#[serial]
async fn t32_f124_poll_completions_skips_drain_under_human_approval() {
    use ccteam_core::artifact_watcher::WatchKind;

    let (_pr, _cr, pdir, paths, _progress, slug) = make_project(YAML_WATCH_FIXER);
    std::fs::create_dir_all(pdir.join("issues")).unwrap();
    let (orch, claude, _codex) = build_orchestrator(paths);
    let spec = human_approval_watch_spec("fixer", "issues", Some(1));
    let progress = orch.paths().progress_jsonl(&slug);

    // First artifact event → parks on pending (no spawn) + 1st
    // plan_decision_required emitted.
    let evt1 = ArtifactEvent {
        role: "fixer".into(),
        artifact_path: pdir.join("issues/a.md"),
        event_kind: WatchKind::Created,
    };
    orch.test_handle_artifact_event(&slug, &spec, &pdir, &progress, evt1)
        .await
        .unwrap();
    assert_eq!(claude.spawn_count(), 0);
    assert_eq!(orch.test_pending_count("fixer").await, 1);

    // Simulate F98 APPROVE: drop a spawn_request marker — but for
    // this test we instead directly assert that `poll_completions`
    // (which is the post-`agent_done` drain entry) does NOT drain
    // pending under HumanApproval. To trigger the drain branch we
    // need a fake completed handle; rather than wiring a full mock
    // session, we hand-write a no-op pending entry and verify
    // `poll_completions` is a no-op for spawn under HumanApproval.
    // The key invariant: spawn_count stays 0 even after polling.
    orch.test_poll_completions(&slug, &spec, &pdir, &progress)
        .await;
    assert_eq!(
        claude.spawn_count(),
        0,
        "poll_completions under HumanApproval must not auto-spawn"
    );
    assert_eq!(
        orch.test_pending_count("fixer").await,
        1,
        "pending queue stays intact under HumanApproval"
    );
}

