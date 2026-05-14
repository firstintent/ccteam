//! V0.4.0 F66 — Thin orchestrator (dispatch shell).
//!
//! Replaces the V0.3.x phase state machine (deleted in F60). Loads
//! `workflow.yaml` (F63) → starts [`ArtifactWatcher`] (F64) → walks
//! initial triggers → event-loops on artifact channel → polls completed
//! sessions via `state.json::status` → fans pending events.
//!
//! ## Architectural red lines (CLAUDE.md §三)
//!
//! - **No prompt injection.** Agent behaviour lives in
//!   `.claude/agents/<role>.md`; this file's grep for `send_prompt` /
//!   `inject_phase` / `phase_prompt` MUST return 0 code hits.
//! - **`progress.jsonl` is SoT.** Every dispatch decision writes one of
//!   the 7 canonical events (workflow_start / agent_spawn / agent_done /
//!   artifact_received / gate_triggered / budget_exceeded / workflow_done).
//! - **Never kill running sessions.** Budget exceeded → block new
//!   spawns only.
//! - **fix-loop 3-strike escalate.** Same role failing `spawn_session`
//!   3 consecutive times → push a `btw` alert to the meta-agent inbox.
//! - **Zero team-name literals.** No `"ccteam"` / `"chainup"` / `"dev"`.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use chrono::Utc;
use serde_json::{json, Value};
use tokio::sync::{mpsc, Mutex};

use crate::artifact_watcher::{ArtifactEvent, ArtifactWatcher};
use crate::harness::{ClaudeCodeAdapter, CodexAdapter, HarnessAdapter, SessionHandle, SpawnOpts};
use crate::paths::CcteamPaths;
use crate::progress;
use crate::workflow::{AgentSpec, Executor, Trigger, WorkflowError, WorkflowSpec};

/// Hard cap on concurrent project sessions (excluding the meta-agent).
pub const MAX_CONCURRENT_PROJECTS: usize = 3;

/// Production model id; `[1m]` opts in to Claude Code's 1M context.
pub const DEFAULT_CLAUDE_MODEL: &str = "claude-sonnet-4-6[1m]";

/// Default budget ceiling (USD). Mirrors CLAUDE.md §三 "项目累计 cost
/// > $200 物理上限". F66 only blocks new spawns at this line — running
/// sessions are never killed.
pub const DEFAULT_BUDGET_LIMIT_USD: f64 = 200.0;

/// Consecutive `spawn_session` failures (per role) before meta-agent
/// escalation. CLAUDE.md §三 fix-loop 3-strike rule.
pub const MAX_CONSECUTIVE_SPAWN_FAILURES: u32 = 3;

/// State.json poll interval for the completion watcher.
pub const COMPLETION_POLL_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct OrchestratorConfig {
    pub tick_interval: Duration,
    pub claude_argv: Vec<String>,
    pub ready_timeout: Duration,
    pub post_ready_warmup: Duration,
    pub skip_tool_check: bool,
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        // F29 — `CCTEAM_CLAUDE_ARGV` lets CLI / e2e harness inject a
        // stub claude without rebuilding the binary.
        let claude_argv = std::env::var("CCTEAM_CLAUDE_ARGV")
            .ok()
            .and_then(|raw| {
                let parts: Vec<String> = raw.split_whitespace().map(String::from).collect();
                if parts.is_empty() {
                    None
                } else {
                    Some(parts)
                }
            })
            .unwrap_or_else(|| {
                vec![
                    "claude".into(),
                    "--dangerously-skip-permissions".into(),
                    "--model".into(),
                    DEFAULT_CLAUDE_MODEL.into(),
                ]
            });
        Self {
            tick_interval: Duration::from_secs(30),
            claude_argv,
            ready_timeout: Duration::from_secs(60),
            post_ready_warmup: Duration::from_secs(3),
            skip_tool_check: false,
        }
    }
}

/// Per-gate lifecycle. F67/F68 may extend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateState {
    Waiting,
    Released,
    Fired,
}

/// V0.4.0 F66 thin orchestrator. Lifecycle-only — never injects prompt.
pub struct Orchestrator {
    paths: CcteamPaths,
    #[allow(dead_code)]
    config: OrchestratorConfig,
    adapters: HashMap<&'static str, Arc<dyn HarnessAdapter + Send + Sync>>,
    running: Arc<Mutex<HashMap<String, Vec<SessionHandle>>>>,
    pending: Arc<Mutex<HashMap<String, VecDeque<ArtifactEvent>>>>,
    fail_counts: Arc<Mutex<HashMap<String, u32>>>,
    gate_states: Arc<Mutex<HashMap<String, GateState>>>,
    cost_accum: Arc<Mutex<f64>>,
}

impl std::fmt::Debug for Orchestrator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Orchestrator")
            .field("paths", &self.paths)
            .field(
                "adapters",
                &self.adapters.keys().copied().collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

enum SessionStatus {
    Running,
    Done {
        cost_usd: Option<f64>,
        status: String,
    },
}

impl Orchestrator {
    /// Build orchestrator; pre-register claude + codex adapters so the
    /// dispatch path never allocates one mid-flight.
    pub fn new(paths: CcteamPaths, config: OrchestratorConfig) -> Result<Self> {
        let mut adapters: HashMap<&'static str, Arc<dyn HarnessAdapter + Send + Sync>> =
            HashMap::new();
        adapters.insert("claude", Arc::new(ClaudeCodeAdapter::new()));
        adapters.insert("codex", Arc::new(CodexAdapter::new()));
        Ok(Self {
            paths,
            config,
            adapters,
            running: Arc::new(Mutex::new(HashMap::new())),
            pending: Arc::new(Mutex::new(HashMap::new())),
            fail_counts: Arc::new(Mutex::new(HashMap::new())),
            gate_states: Arc::new(Mutex::new(HashMap::new())),
            cost_accum: Arc::new(Mutex::new(0.0)),
        })
    }

    pub fn paths(&self) -> &CcteamPaths {
        &self.paths
    }

    fn adapter_for(&self, exec: Executor) -> Option<&Arc<dyn HarnessAdapter + Send + Sync>> {
        let key: &'static str = match exec {
            Executor::Claude => "claude",
            Executor::Codex => "codex",
        };
        self.adapters.get(key)
    }

    /// Test-only adapter override; production CLI never calls this.
    #[cfg(any(test, feature = "test-util"))]
    pub fn set_adapter(&mut self, exec: Executor, adapter: Arc<dyn HarnessAdapter + Send + Sync>) {
        let key: &'static str = match exec {
            Executor::Claude => "claude",
            Executor::Codex => "codex",
        };
        self.adapters.insert(key, adapter);
    }

    // ---- public entry points ------------------------------------------------

    pub async fn run_project(&self, slug: &str) -> Result<()> {
        let project_dir = self.paths.project_dir(slug);
        let progress_path = self.paths.progress_jsonl(slug);

        let spec = WorkflowSpec::load_for_project(&project_dir).map_err(|e| match e {
            WorkflowError::NotFound(p) => anyhow::anyhow!("workflow.yaml not found in {:?}", p),
            other => anyhow::anyhow!(other),
        })?;

        progress::append_event(
            &progress_path,
            &json!({
                "event": "workflow_start",
                "workflow": spec.name,
                "slug": slug,
                "ts": Utc::now().to_rfc3339(),
            }),
        )?;

        let (watcher, rx) = ArtifactWatcher::new(&spec, Some(project_dir.as_path()))?;
        let watcher_handle = watcher.start();

        self.dispatch_initial_triggers(slug, &spec).await?;
        let res = self
            .event_loop(slug, &spec, &project_dir, &progress_path, rx)
            .await;

        watcher_handle.abort();
        res
    }

    /// F66 daemon loop = park until shutdown. Per-project iteration +
    /// inbox drain land in F67 once meta-agent MCP tooling sits on top.
    pub async fn run<F>(&self, shutdown: F) -> Result<()>
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        tracing::info!("orchestrator daemon: parked (F66 stub; per-project via run_project)");
        shutdown.await;
        Ok(())
    }

    // ---- dispatch helpers ---------------------------------------------------

    async fn dispatch_initial_triggers(&self, slug: &str, spec: &WorkflowSpec) -> Result<()> {
        for (role, agent) in &spec.agents {
            match &agent.trigger {
                Trigger::Manual | Trigger::Schedule => {
                    tracing::info!(slug, role = role.as_str(), "waiting for explicit trigger");
                }
                Trigger::Gate => {
                    self.gate_states
                        .lock()
                        .await
                        .insert(role.clone(), GateState::Waiting);
                    tracing::info!(slug, role = role.as_str(), "gate waiting");
                }
                Trigger::Watch(path) => {
                    tracing::info!(slug, role = role.as_str(), watch = ?path, "watch registered");
                }
            }
        }
        Ok(())
    }

    async fn event_loop(
        &self,
        slug: &str,
        spec: &WorkflowSpec,
        project_dir: &std::path::Path,
        progress_path: &std::path::Path,
        mut rx: mpsc::Receiver<ArtifactEvent>,
    ) -> Result<()> {
        let mut ticker = tokio::time::interval(COMPLETION_POLL_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                maybe = rx.recv() => match maybe {
                    Some(evt) => {
                        if let Err(err) = self
                            .handle_artifact_event(slug, spec, project_dir, progress_path, evt)
                            .await
                        {
                            tracing::warn!(?err, slug, "handle_artifact_event failed");
                        }
                    }
                    None => {
                        tracing::info!(slug, "artifact channel closed; loop done");
                        break;
                    }
                },
                _ = ticker.tick() => {
                    self.poll_completions(slug, spec, project_dir, progress_path).await;
                    self.check_spawn_requests(slug, spec, project_dir, progress_path).await;
                    self.check_gates(slug, spec, project_dir, progress_path).await;
                    self.check_workflow_done(slug, spec, progress_path).await;
                }
            }
        }
        Ok(())
    }

    async fn handle_artifact_event(
        &self,
        slug: &str,
        spec: &WorkflowSpec,
        project_dir: &std::path::Path,
        progress_path: &std::path::Path,
        evt: ArtifactEvent,
    ) -> Result<()> {
        let role = evt.role.clone();
        let Some(agent) = spec.agents.get(&role) else {
            tracing::warn!(role = role.as_str(), "artifact event for unknown role");
            return Ok(());
        };

        progress::append_event(
            progress_path,
            &json!({
                "event": "artifact_received",
                "role": role,
                "artifact_path": evt.artifact_path,
                "slug": slug,
                "ts": Utc::now().to_rfc3339(),
            }),
        )?;

        let max_par = agent.parallelism.unwrap_or(1).max(1) as usize;
        let running_count = self
            .running
            .lock()
            .await
            .get(&role)
            .map(|v| v.len())
            .unwrap_or(0);

        if running_count >= max_par {
            self.pending
                .lock()
                .await
                .entry(role.clone())
                .or_default()
                .push_back(evt);
            return Ok(());
        }

        self.try_spawn(slug, &role, agent, project_dir, progress_path)
            .await
    }

    async fn try_spawn(
        &self,
        slug: &str,
        role: &str,
        agent: &AgentSpec,
        project_dir: &std::path::Path,
        progress_path: &std::path::Path,
    ) -> Result<()> {
        // Budget guard — progress.jsonl is SoT, so cumulative cost
        // survives orchestrator restarts.
        let cost_so_far = self
            .cumulative_cost_from_progress(progress_path)
            .await
            .unwrap_or(0.0);
        let budget = self.budget_limit_for_project(project_dir);
        if cost_so_far >= budget {
            progress::append_event(
                progress_path,
                &json!({
                    "event": "budget_exceeded",
                    "role": role,
                    "cost_used_usd": cost_so_far,
                    "budget_limit_usd": budget,
                    "slug": slug,
                    "ts": Utc::now().to_rfc3339(),
                }),
            )?;
            self.send_btw_escalation(
                slug,
                &format!(
                    "budget exceeded for role `{}` (used ${:.2} of ${:.2}); spawn blocked. \
                     Running sessions left intact.",
                    role, cost_so_far, budget
                ),
            )
            .await;
            return Ok(());
        }

        let Some(adapter) = self.adapter_for(agent.executor) else {
            tracing::warn!(role, executor = ?agent.executor, "no adapter registered");
            self.bump_fail_count(slug, role, progress_path).await?;
            return Ok(());
        };

        let sid = format!("{}-{}", role, self.next_role_seq().await);
        let opts = SpawnOpts {
            harness: match agent.executor {
                Executor::Claude => "claude-code",
                Executor::Codex => "codex",
            },
            slug: slug.to_string(),
            sid: sid.clone(),
            cwd: project_dir.to_path_buf(),
            role: role.to_string(),
            extra_args: Vec::new(),
        };

        match adapter.spawn_session(opts) {
            Ok(handle) => {
                self.running
                    .lock()
                    .await
                    .entry(role.to_string())
                    .or_default()
                    .push(handle.clone());
                self.fail_counts.lock().await.insert(role.to_string(), 0);
                progress::append_event(
                    progress_path,
                    &json!({
                        "event": "agent_spawn",
                        "role": role,
                        "session_id": handle.sid,
                        "tmux_session": handle.tmux_session,
                        "executor": match agent.executor {
                            Executor::Claude => "claude",
                            Executor::Codex => "codex",
                        },
                        "slug": slug,
                        "ts": Utc::now().to_rfc3339(),
                    }),
                )?;
                Ok(())
            }
            Err(err) => {
                tracing::warn!(role, ?err, "spawn_session failed");
                self.bump_fail_count(slug, role, progress_path).await?;
                Ok(())
            }
        }
    }

    /// Monotonic microsecond sequence — collision-free across one
    /// orchestrator instance; F67 may swap in a counter map.
    async fn next_role_seq(&self) -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_micros() as u64)
            .unwrap_or(1)
    }

    async fn bump_fail_count(
        &self,
        slug: &str,
        role: &str,
        progress_path: &std::path::Path,
    ) -> Result<()> {
        let cur = {
            let mut counts = self.fail_counts.lock().await;
            let entry = counts.entry(role.to_string()).or_insert(0);
            *entry = entry.saturating_add(1);
            *entry
        };

        progress::append_event(
            progress_path,
            &json!({
                "event": "escalation",
                "kind": "spawn_failed",
                "role": role,
                "consecutive_failures": cur,
                "slug": slug,
                "ts": Utc::now().to_rfc3339(),
            }),
        )?;

        if cur >= MAX_CONSECUTIVE_SPAWN_FAILURES {
            self.send_btw_escalation(
                slug,
                &format!(
                    "role `{}` failed to spawn {} consecutive times — orchestrator escalating \
                     per fix-loop 3-strike rule.",
                    role, cur
                ),
            )
            .await;
        }
        Ok(())
    }

    async fn poll_completions(
        &self,
        slug: &str,
        spec: &WorkflowSpec,
        project_dir: &std::path::Path,
        progress_path: &std::path::Path,
    ) {
        let mut finished: Vec<(String, SessionHandle, Option<f64>, String)> = Vec::new();
        {
            let mut running = self.running.lock().await;
            for (role, handles) in running.iter_mut() {
                let mut keep = Vec::with_capacity(handles.len());
                for handle in handles.drain(..) {
                    match self.session_status(&handle) {
                        SessionStatus::Done { cost_usd, status } => {
                            finished.push((role.clone(), handle, cost_usd, status));
                        }
                        SessionStatus::Running => keep.push(handle),
                    }
                }
                *handles = keep;
            }
            running.retain(|_, v| !v.is_empty());
        }

        for (role, handle, cost_usd, status) in finished {
            if let Some(c) = cost_usd {
                *self.cost_accum.lock().await += c;
            }
            let _ = progress::append_event(
                progress_path,
                &json!({
                    "event": "agent_done",
                    "role": role,
                    "session_id": handle.sid,
                    "status": status,
                    "cost_usd": cost_usd.unwrap_or(0.0),
                    "slug": slug,
                    "ts": Utc::now().to_rfc3339(),
                }),
            );

            if status == "completed" || status == "stopped" {
                self.fail_counts.lock().await.insert(role.clone(), 0);
            }

            // Drain one pending event for this role, if any.
            let pending_evt = {
                let mut pending = self.pending.lock().await;
                pending.get_mut(&role).and_then(|q| q.pop_front())
            };
            if let Some(evt) = pending_evt {
                if let Some(agent) = spec.agents.get(&role) {
                    let _ = self
                        .try_spawn(slug, &role, agent, project_dir, progress_path)
                        .await;
                    let _ = evt;
                }
            }
        }

        self.check_gates(slug, spec, project_dir, progress_path)
            .await;
    }

    /// Scan `.ccteam/spawn_requests/*.json` markers written by the F65
    /// `ccteam__spawn_agent` MCP tool (or hand-written by users via the
    /// migration-guide path). Each marker JSON carries `{"role": "...",
    /// ...}`; orchestrator spawns the role and deletes the marker on
    /// success. Failed spawns retain the marker for the next tick so
    /// transient errors auto-retry (subject to fix-loop 3-strike
    /// escalate per `bump_fail_count`).
    ///
    /// This is the V0.4.0 wiring for `Trigger::Manual` / `Trigger::Schedule`
    /// agents — they have no natural fire path otherwise. `Trigger::Gate`
    /// still goes through `check_gates`, and `Trigger::Watch` through the
    /// inotify-driven `handle_artifact_event`. Manual roles are spawned
    /// regardless of trigger kind once a marker shows up (lets users
    /// force-fire any role for debug / replay).
    async fn check_spawn_requests(
        &self,
        slug: &str,
        spec: &WorkflowSpec,
        project_dir: &std::path::Path,
        progress_path: &std::path::Path,
    ) {
        let bucket = project_dir.join(".ccteam").join("spawn_requests");
        let Ok(entries) = std::fs::read_dir(&bucket) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let role = match std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .and_then(|v| v.get("role").and_then(|r| r.as_str()).map(String::from))
            {
                Some(r) => r,
                None => {
                    tracing::warn!(
                        marker = ?path,
                        "spawn_request missing `role` field; deleting"
                    );
                    let _ = std::fs::remove_file(&path);
                    continue;
                }
            };
            let Some(agent) = spec.agents.get(&role) else {
                tracing::warn!(role, "spawn_request for unknown role; deleting");
                let _ = std::fs::remove_file(&path);
                continue;
            };
            // `try_spawn` swallows spawn errors internally (bumps
            // fail_count + logs) and returns Ok(()), so we sample the
            // fail counter to decide whether the marker should be kept
            // for next-tick retry. The marker is deleted ONLY when a
            // session actually came up; transient errors retain it.
            let before = self
                .fail_counts
                .lock()
                .await
                .get(&role)
                .copied()
                .unwrap_or(0);
            if let Err(err) = self
                .try_spawn(slug, &role, agent, project_dir, progress_path)
                .await
            {
                tracing::warn!(role, error = ?err, "spawn_request errored");
                continue;
            }
            let after = self
                .fail_counts
                .lock()
                .await
                .get(&role)
                .copied()
                .unwrap_or(0);
            if after > before {
                tracing::warn!(
                    role,
                    "spawn_request retained for retry (fail_count {} → {})",
                    before,
                    after
                );
                continue;
            }
            let _ = std::fs::remove_file(&path);
        }
    }

    /// Release a `Trigger::Gate` agent when its input dir has ≥1 file
    /// (default policy; richer thresholds land in F67) OR an explicit
    /// `.ccteam/gate_override/<role>` file exists (force).
    async fn check_gates(
        &self,
        slug: &str,
        spec: &WorkflowSpec,
        project_dir: &std::path::Path,
        progress_path: &std::path::Path,
    ) {
        for (role, agent) in &spec.agents {
            if !matches!(agent.trigger, Trigger::Gate) {
                continue;
            }
            let cur = self
                .gate_states
                .lock()
                .await
                .get(role)
                .copied()
                .unwrap_or(GateState::Waiting);
            if cur != GateState::Waiting {
                continue;
            }

            let override_path = project_dir.join(".ccteam").join("gate_override").join(role);
            let forced = override_path.exists();
            let threshold_met = agent
                .input
                .as_ref()
                .map(|rel| {
                    let dir = project_dir.join(rel);
                    std::fs::read_dir(&dir)
                        .map(|rd| rd.flatten().any(|e| e.path().is_file()))
                        .unwrap_or(false)
                })
                .unwrap_or(false);

            if forced || threshold_met {
                self.gate_states
                    .lock()
                    .await
                    .insert(role.clone(), GateState::Released);
                let _ = progress::append_event(
                    progress_path,
                    &json!({
                        "event": "gate_triggered",
                        "role": role,
                        "forced": forced,
                        "threshold_met": threshold_met,
                        "slug": slug,
                        "ts": Utc::now().to_rfc3339(),
                    }),
                );
                if forced {
                    let _ = std::fs::remove_file(&override_path);
                }
                let _ = self
                    .try_spawn(slug, role, agent, project_dir, progress_path)
                    .await;
                self.gate_states
                    .lock()
                    .await
                    .insert(role.clone(), GateState::Fired);
            }
        }
    }

    /// Idempotent: emits `workflow_done` exactly once when every gate
    /// agent is Fired AND has no running session. Uses sentinel key
    /// `__workflow_done__` to guard against double-emit.
    async fn check_workflow_done(
        &self,
        slug: &str,
        spec: &WorkflowSpec,
        progress_path: &std::path::Path,
    ) {
        const SENTINEL: &str = "__workflow_done__";
        if self.gate_states.lock().await.contains_key(SENTINEL) {
            return;
        }
        let mut any_gate = false;
        let mut all_fired = true;
        for (role, agent) in &spec.agents {
            if !matches!(agent.trigger, Trigger::Gate) {
                continue;
            }
            any_gate = true;
            let state = self
                .gate_states
                .lock()
                .await
                .get(role)
                .copied()
                .unwrap_or(GateState::Waiting);
            let running_count = self
                .running
                .lock()
                .await
                .get(role)
                .map(|v| v.len())
                .unwrap_or(0);
            if state != GateState::Fired || running_count > 0 {
                all_fired = false;
                break;
            }
        }
        if any_gate && all_fired {
            self.gate_states
                .lock()
                .await
                .insert(SENTINEL.to_string(), GateState::Fired);
            let _ = progress::append_event(
                progress_path,
                &json!({
                    "event": "workflow_done",
                    "workflow": spec.name,
                    "slug": slug,
                    "ts": Utc::now().to_rfc3339(),
                }),
            );
        }
    }

    fn session_status(&self, handle: &SessionHandle) -> SessionStatus {
        let path = self.session_state_path(handle);
        let Ok(body) = std::fs::read_to_string(&path) else {
            return SessionStatus::Running;
        };
        let Ok(v) = serde_json::from_str::<Value>(&body) else {
            return SessionStatus::Running;
        };
        let status = v
            .get("status")
            .and_then(|s| s.as_str())
            .unwrap_or("running");
        if !matches!(status, "stopped" | "completed" | "error") {
            return SessionStatus::Running;
        }
        let cost_usd = v
            .get("cost_usd")
            .and_then(|n| n.as_f64())
            .or_else(|| v.get("cost").and_then(|n| n.as_f64()));
        SessionStatus::Done {
            cost_usd,
            status: status.to_string(),
        }
    }

    /// State.json path: claude → `~/.claude/jobs/<sid>/state.json`,
    /// codex → `~/.ccteam/codex/<sid>/state.json`. Tests override both
    /// via `CCTEAM_SESSION_STATE_DIR`.
    fn session_state_path(&self, handle: &SessionHandle) -> PathBuf {
        if let Ok(custom) = std::env::var("CCTEAM_SESSION_STATE_DIR") {
            return PathBuf::from(custom).join(&handle.sid).join("state.json");
        }
        if handle.harness == "codex" {
            return self
                .paths
                .root
                .join("codex")
                .join(&handle.sid)
                .join("state.json");
        }
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
        home.join(".claude")
            .join("jobs")
            .join(&handle.sid)
            .join("state.json")
    }

    /// Sum `cost_usd` over every `agent_done` row in `progress.jsonl`.
    /// progress.jsonl is the SoT; the in-process accumulator is just a
    /// floor for the race-against-hook-writer case.
    async fn cumulative_cost_from_progress(&self, progress_path: &std::path::Path) -> Result<f64> {
        let events = progress::read_all_events(progress_path).unwrap_or_default();
        let mut total = 0.0;
        for evt in events {
            if evt.get("event").and_then(|s| s.as_str()) == Some("agent_done") {
                if let Some(c) = evt.get("cost_usd").and_then(|n| n.as_f64()) {
                    total += c;
                }
            }
        }
        Ok(total.max(*self.cost_accum.lock().await))
    }

    /// V0.4.0 F66 budget resolution: env `CCTEAM_BUDGET_LIMIT_USD`
    /// (test hook) → [`DEFAULT_BUDGET_LIMIT_USD`]. F67 will read
    /// `team.yaml::cost.hard_kill_threshold_usd` here.
    fn budget_limit_for_project(&self, _project_dir: &std::path::Path) -> f64 {
        std::env::var("CCTEAM_BUDGET_LIMIT_USD")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(DEFAULT_BUDGET_LIMIT_USD)
    }

    /// Push a meta-agent `btw` escalation file into the inbox dir. F67
    /// may wire a richer routing path; F66 keeps it as a flat write.
    async fn send_btw_escalation(&self, slug: &str, body: &str) {
        let target_dir = self.paths.inbox_dir();
        if std::fs::create_dir_all(&target_dir).is_err() {
            tracing::warn!(slug, "could not create inbox dir for escalation");
            return;
        }
        let fname = format!(
            "{}-escalation-{}.txt",
            Utc::now().format("%Y%m%dT%H%M%SZ"),
            slug
        );
        let path = target_dir.join(fname);
        if let Err(err) = std::fs::write(&path, body) {
            tracing::warn!(slug, ?err, "escalation write failed");
        } else {
            tracing::info!(slug, body, "escalation queued");
        }
    }
}

// =====================================================================
// Test surface (gated behind `cfg(any(test, feature = "test-util"))`).
// Lets the integration crate at `tests/orchestrator_thin_test.rs` drive
// the orchestrator without tmux. All methods are prefixed `test_*`
// (plus `set_adapter`) — production CLI never calls them.
// =====================================================================
#[cfg(any(test, feature = "test-util"))]
impl Orchestrator {
    pub async fn test_handle_artifact_event(
        &self,
        slug: &str,
        spec: &WorkflowSpec,
        project_dir: &std::path::Path,
        progress_path: &std::path::Path,
        evt: ArtifactEvent,
    ) -> Result<()> {
        self.handle_artifact_event(slug, spec, project_dir, progress_path, evt)
            .await
    }

    pub async fn test_running_count(&self, role: &str) -> usize {
        self.running
            .lock()
            .await
            .get(role)
            .map(|v| v.len())
            .unwrap_or(0)
    }

    pub async fn test_pending_count(&self, role: &str) -> usize {
        self.pending
            .lock()
            .await
            .get(role)
            .map(|q| q.len())
            .unwrap_or(0)
    }

    pub async fn test_fail_count(&self, role: &str) -> u32 {
        self.fail_counts
            .lock()
            .await
            .get(role)
            .copied()
            .unwrap_or(0)
    }

    pub async fn test_poll_completions(
        &self,
        slug: &str,
        spec: &WorkflowSpec,
        project_dir: &std::path::Path,
        progress_path: &std::path::Path,
    ) {
        self.poll_completions(slug, spec, project_dir, progress_path)
            .await;
        self.check_workflow_done(slug, spec, progress_path).await;
    }

    pub async fn test_check_spawn_requests(
        &self,
        slug: &str,
        spec: &WorkflowSpec,
        project_dir: &std::path::Path,
        progress_path: &std::path::Path,
    ) {
        self.check_spawn_requests(slug, spec, project_dir, progress_path)
            .await;
    }

    pub async fn test_gate_override(
        &self,
        slug: &str,
        spec: &WorkflowSpec,
        role: &str,
        project_dir: &std::path::Path,
        progress_path: &std::path::Path,
    ) -> Result<()> {
        let dir = project_dir.join(".ccteam").join("gate_override");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join(role), "force")?;
        self.check_gates(slug, spec, project_dir, progress_path)
            .await;
        Ok(())
    }

    pub async fn test_register_running(&self, role: &str, handle: SessionHandle) {
        self.running
            .lock()
            .await
            .entry(role.to_string())
            .or_default()
            .push(handle);
    }

    pub fn test_adapter_keys(&self) -> Vec<&'static str> {
        let mut keys: Vec<_> = self.adapters.keys().copied().collect();
        keys.sort();
        keys
    }
}
