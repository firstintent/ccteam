//! V0.6.3 F145 — squad cross-session runtime routing integration tests.
//!
//! Exercises the orchestrator's `handle_artifact_event` squad-route
//! branch without tmux: a `MockAdapter` records every spawn, and
//! synthesised [`ArtifactEvent`]s tagged with the squad route sentinel
//! are fed through the `test_handle_artifact_event` hook.
//!
//! Coverage:
//! - leader writes `<member>--*.md` → the named member spawns;
//! - a different prefix routes to a different member;
//! - a prefix not in `squad.members` → `escalation` (no spawn);
//! - exceeding `hop_limit` → `escalation` (no spawn);
//! - `progress.jsonl` is asserted as the SoT for every decision.

#![cfg(feature = "test-util")]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use serde_json::Value;
use tempfile::TempDir;

use ccteam_core::artifact_watcher::{ArtifactEvent, WatchKind};
use ccteam_core::orchestrator::{Orchestrator, OrchestratorConfig};
use ccteam_core::workflow::{
    AgentSpec, Executor, SquadSpec, Trigger, WorkflowSpec, SQUAD_ROUTE_SENTINEL,
};
use ccteam_core::CcteamPaths;
use ccteam_harness::{
    AgentSpecBrief, AgentVendor, ExecutionMode, HarnessAdapter, HarnessError, SpawnCtx,
    ThreadEvent, ThreadHandle, TurnId, TurnInput,
};
use futures::stream::{self, BoxStream};

// =====================================================================
// MockAdapter — minimal HarnessAdapter test double.
// =====================================================================

#[derive(Clone, Default)]
struct MockAdapter {
    name: &'static str,
    /// Roles every successful `start_thread` spawned.
    spawned_roles: Arc<Mutex<Vec<String>>>,
    seq: Arc<AtomicU64>,
}

impl MockAdapter {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            seq: Arc::new(AtomicU64::new(1)),
            ..Default::default()
        }
    }
    fn spawned(&self) -> Vec<String> {
        self.spawned_roles.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl HarnessAdapter for MockAdapter {
    fn name(&self) -> &'static str {
        self.name
    }

    fn vendor(&self) -> AgentVendor {
        AgentVendor::Claude
    }

    async fn start_thread(
        &self,
        spec: &AgentSpecBrief,
        _ctx: &SpawnCtx,
    ) -> Result<ThreadHandle, HarnessError> {
        self.spawned_roles.lock().unwrap().push(spec.role.clone());
        let n = self.seq.fetch_add(1, Ordering::SeqCst);
        Ok(ThreadHandle {
            vendor: AgentVendor::Claude,
            mode: ExecutionMode::Bg,
            identity: format!("mock-job-{n}"),
            started_at: chrono::Utc::now(),
            raw_extras: serde_json::json!({
                "tmux_session": format!("mock-tmux-{n}"),
                "pid": 20_000u64 + n,
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
        Ok(())
    }
}

// =====================================================================
// Fixtures
// =====================================================================

fn squad_spec() -> WorkflowSpec {
    let mut agents = IndexMap::new();
    for role in ["coordinator", "backend", "frontend"] {
        agents.insert(
            role.to_string(),
            AgentSpec {
                executor: Executor::Claude,
                model: None,
                trigger: Trigger::Manual,
                scope: None,
                parallelism: None,
                input: None,
                output: None,
                schedule: None,
                timeout: None,
                on_timeout: None,
                plan_approval: None,
                chat_handle: None,
            },
        );
    }
    WorkflowSpec {
        name: "squad-routed".into(),
        description: None,
        mode: ccteam_core::WorkflowMode::default(),
        enabled: true,
        budget: None,
        budgets_v060: None,
        agent_team: None,
        chat: None,
        squad: Some(SquadSpec {
            leader: "coordinator".into(),
            members: vec!["backend".into(), "frontend".into()],
            hop_limit: 3,
        }),
        agents,
    }
}

fn make_project() -> (TempDir, TempDir, PathBuf, CcteamPaths, PathBuf, String) {
    let projects_root = tempfile::tempdir().unwrap();
    let ccteam_root = tempfile::tempdir().unwrap();
    let slug = format!(
        "f143-{}",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    );
    let project_dir = projects_root.path().join(&slug);
    std::fs::create_dir_all(&project_dir).unwrap();
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

fn build_orchestrator(paths: CcteamPaths) -> (Orchestrator, Arc<MockAdapter>) {
    let mut orch = Orchestrator::new(paths, OrchestratorConfig::default()).unwrap();
    let claude_mock = Arc::new(MockAdapter::new("claude-mock"));
    orch.set_adapter(Executor::Claude, claude_mock.clone());
    (orch, claude_mock)
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

/// Build a squad routing event: a file `<file_name>` dropped into the
/// project's `.ccteam/squad/` dir, tagged with the route sentinel.
fn squad_event(project_dir: &Path, file_name: &str) -> ArtifactEvent {
    ArtifactEvent {
        role: SQUAD_ROUTE_SENTINEL.to_string(),
        artifact_path: project_dir.join(".ccteam/squad").join(file_name),
        event_kind: WatchKind::Created,
    }
}

// =====================================================================
// Tests
// =====================================================================

#[tokio::test]
async fn leader_routes_to_backend_member() {
    let (_pr, _cr, pdir, paths, _pp, slug) = make_project();
    let (orch, claude) = build_orchestrator(paths);
    let spec = squad_spec();
    let progress = orch.paths().progress_jsonl(&slug);

    orch.test_handle_artifact_event(
        &slug,
        &spec,
        &pdir,
        &progress,
        squad_event(&pdir, "backend--task.md"),
    )
    .await
    .unwrap();

    assert_eq!(claude.spawned(), vec!["backend".to_string()]);
    assert_eq!(orch.test_running_count("backend").await, 1);

    let events = read_events(&progress);
    assert!(events.iter().any(|e| {
        e.get("event").and_then(Value::as_str) == Some("agent_spawn")
            && e.get("role").and_then(Value::as_str) == Some("backend")
    }));
}

#[tokio::test]
async fn leader_routes_to_frontend_member() {
    let (_pr, _cr, pdir, paths, _pp, slug) = make_project();
    let (orch, claude) = build_orchestrator(paths);
    let spec = squad_spec();
    let progress = orch.paths().progress_jsonl(&slug);

    orch.test_handle_artifact_event(
        &slug,
        &spec,
        &pdir,
        &progress,
        squad_event(&pdir, "frontend--build-ui.md"),
    )
    .await
    .unwrap();

    assert_eq!(claude.spawned(), vec!["frontend".to_string()]);
    assert_eq!(orch.test_running_count("frontend").await, 1);
    assert_eq!(orch.test_running_count("backend").await, 0);
}

#[tokio::test]
async fn unknown_target_prefix_escalates_without_spawn() {
    let (_pr, _cr, pdir, paths, _pp, slug) = make_project();
    let (orch, claude) = build_orchestrator(paths);
    let spec = squad_spec();
    let progress = orch.paths().progress_jsonl(&slug);

    // `database` is not a declared squad member.
    orch.test_handle_artifact_event(
        &slug,
        &spec,
        &pdir,
        &progress,
        squad_event(&pdir, "database--migrate.md"),
    )
    .await
    .unwrap();

    assert!(claude.spawned().is_empty(), "unknown target must not spawn");
    let events = read_events(&progress);
    assert!(events.iter().any(|e| {
        e.get("event").and_then(Value::as_str) == Some("escalation")
            && e.get("kind").and_then(Value::as_str) == Some("squad_unknown_target")
    }));
}

#[tokio::test]
async fn hop_limit_exceeded_escalates_without_spawn() {
    let (_pr, _cr, pdir, paths, _pp, slug) = make_project();
    let (orch, claude) = build_orchestrator(paths);
    let spec = squad_spec(); // hop_limit = 3
    let progress = orch.paths().progress_jsonl(&slug);

    // hop 3 == hop_limit → escalate.
    orch.test_handle_artifact_event(
        &slug,
        &spec,
        &pdir,
        &progress,
        squad_event(&pdir, "backend--h3--retry.md"),
    )
    .await
    .unwrap();

    assert!(
        claude.spawned().is_empty(),
        "routed file at hop_limit must not spawn"
    );
    let events = read_events(&progress);
    let esc = events
        .iter()
        .find(|e| {
            e.get("event").and_then(Value::as_str) == Some("escalation")
                && e.get("kind").and_then(Value::as_str) == Some("squad_hop_limit")
        })
        .expect("squad_hop_limit escalation event");
    assert_eq!(esc.get("hop").and_then(Value::as_u64), Some(3));
    assert_eq!(esc.get("hop_limit").and_then(Value::as_u64), Some(3));
}

#[tokio::test]
async fn hop_below_limit_still_spawns() {
    let (_pr, _cr, pdir, paths, _pp, slug) = make_project();
    let (orch, claude) = build_orchestrator(paths);
    let spec = squad_spec(); // hop_limit = 3
    let progress = orch.paths().progress_jsonl(&slug);

    // hop 2 < hop_limit 3 → still routes.
    orch.test_handle_artifact_event(
        &slug,
        &spec,
        &pdir,
        &progress,
        squad_event(&pdir, "backend--h2--retry.md"),
    )
    .await
    .unwrap();

    assert_eq!(claude.spawned(), vec!["backend".to_string()]);
}

#[tokio::test]
async fn non_routed_filename_is_ignored() {
    let (_pr, _cr, pdir, paths, _pp, slug) = make_project();
    let (orch, claude) = build_orchestrator(paths);
    let spec = squad_spec();
    let progress = orch.paths().progress_jsonl(&slug);

    // No `<member>--` prefix → not a routed artifact, no spawn, no
    // escalation.
    orch.test_handle_artifact_event(
        &slug,
        &spec,
        &pdir,
        &progress,
        squad_event(&pdir, "stray-note.md"),
    )
    .await
    .unwrap();

    assert!(claude.spawned().is_empty());
    let events = read_events(&progress);
    assert!(!events
        .iter()
        .any(|e| e.get("event").and_then(Value::as_str) == Some("escalation")));
}
