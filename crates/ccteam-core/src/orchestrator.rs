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

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use chrono::Utc;
use serde_json::{json, Value};
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinSet;

use crate::artifact_watcher::{ArtifactEvent, ArtifactWatcher};
use crate::daemon;
use crate::harness::{ClaudeCodeAdapter, CodexAdapter, HarnessAdapter, SessionHandle, SpawnOpts};
use crate::inbox::{InboxMessage, SessionMailbox};
use crate::paths::CcteamPaths;
use crate::progress;
use crate::queries;
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

/// How often the daemon rescans `paths.projects_root` to pick up
/// projects created after startup (e.g. via `ccteam init` or `ccteam
/// new` while the daemon is running). Each rescan walks the dir and
/// spawns event loops for any slug it hasn't seen before.
pub const ROSTER_RESCAN_INTERVAL: Duration = Duration::from_secs(10);

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

/// Loose probe for two optional routing hints in the inbox front matter
/// that the strict `InboxMessage` schema doesn't model:
///   `target_role: <role>`  — auto-spawn target for the message
///   `no_spawn: true`       — archive only, skip auto-spawn
///
/// Returns `(target_role, no_spawn_opt_out)`. Missing front matter or
/// any parse failure → `(None, false)` (default behaviour: route to
/// first manual role).
fn parse_inbox_routing_hints(raw: &str) -> (Option<String>, bool) {
    let after_first = match raw
        .strip_prefix("---\n")
        .or_else(|| raw.strip_prefix("---\r\n"))
    {
        Some(s) => s,
        None => return (None, false),
    };
    let end = match after_first
        .find("\n---\n")
        .or_else(|| after_first.find("\n---\r\n"))
    {
        Some(i) => i,
        None => return (None, false),
    };
    let yaml = &after_first[..end];
    let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(yaml) else {
        return (None, false);
    };
    let target_role = value
        .get("target_role")
        .and_then(|v| v.as_str())
        .map(String::from);
    let no_spawn = value
        .get("no_spawn")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    (target_role, no_spawn)
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

        let mut spec = WorkflowSpec::load_for_project(&project_dir).map_err(|e| match e {
            WorkflowError::NotFound(p) => anyhow::anyhow!("workflow.yaml not found in {:?}", p),
            other => anyhow::anyhow!(other),
        })?;

        // V0.4.5: workflow.yaml `watch:<rel>` paths are project-relative
        // per PRD §6.1, but `ArtifactWatcher::new` (per its docstring)
        // treats them literally. The previous version of this function
        // passed the spec straight through, so the watcher installed
        // inotify on `<daemon-cwd>/.ccteam/backlog/` (typically the
        // ccteam repo) instead of `<project>/.ccteam/backlog/`. Writes
        // inside the actual project went unnoticed and downstream
        // `watch:` agents never triggered. Rewrite every relative
        // Trigger::Watch path to absolute under project_dir before
        // handing the spec to the watcher.
        for (_role, agent) in spec.agents.iter_mut() {
            if let Trigger::Watch(path) = &mut agent.trigger {
                if !path.is_absolute() {
                    *path = project_dir.join(&*path);
                }
            }
        }

        progress::append_event(
            &progress_path,
            &json!({
                "event": "workflow_start",
                "workflow": spec.name,
                "slug": slug,
                "ts": Utc::now().to_rfc3339(),
            }),
        )?;

        // V0.4.5: pass the progress.jsonl file (so the watcher can
        // append `artifact_dir_created` events) — the previous version
        // passed `project_dir` (a directory), which made
        // `progress::append_event` fail with "open <project_dir>".
        let (watcher, rx) = ArtifactWatcher::new(&spec, Some(progress_path.as_path()))?;
        let watcher_handle = watcher.start();

        self.dispatch_initial_triggers(slug, &spec).await?;
        let res = self
            .event_loop(slug, &spec, &project_dir, &progress_path, rx)
            .await;

        watcher_handle.abort();
        res
    }

    /// Daemon entry point: roster every project under
    /// `paths.projects_root` that has a `workflow.yaml`, drive each
    /// through [`run_project`] on its own tokio task, and keep the
    /// heartbeat file fresh so [`crate::daemon::check_health`] reports
    /// healthy. Returns when `shutdown` resolves.
    ///
    /// ### Hot-reload of new projects
    ///
    /// `paths.projects_root` is rescanned every [`ROSTER_RESCAN_INTERVAL`]
    /// so projects created after startup (e.g. `ccteam init` running
    /// against a live daemon) get an event loop without requiring a
    /// daemon restart. A slug is spawned at most once per daemon
    /// lifetime — completed/failed tasks are NOT respawned (matches
    /// existing "next `ccteam start` re-rosters" semantics).
    ///
    /// ### Shutdown semantics
    ///
    /// On `shutdown`, per-project tasks are aborted via `JoinSet`.
    /// Aborting a tokio task does **not** kill the underlying
    /// claude/codex sessions it spawned — those keep running as their
    /// own processes (CLAUDE.md §三 "永不主动 kill 长 session"). The
    /// next `ccteam start` re-rosters them via state.json polling.
    ///
    /// ### `self: &Arc<Self>`
    ///
    /// Each per-project task captures an `Arc<Self>` clone so it owns a
    /// 'static reference to the orchestrator. All internal state is
    /// already `Arc<Mutex<...>>` so concurrent project tasks share it
    /// safely.
    pub async fn run<F>(self: &Arc<Self>, shutdown: F) -> Result<()>
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let mut tasks: JoinSet<(String, Result<()>)> = JoinSet::new();
        let mut spawned: HashSet<String> = HashSet::new();

        self.spawn_new_rostered_projects(&mut tasks, &mut spawned, "startup");

        if let Err(err) = daemon::write_heartbeat(&self.paths) {
            tracing::warn!(?err, "initial heartbeat write failed");
        }
        let mut hb_ticker = tokio::time::interval(daemon::HEARTBEAT_INTERVAL);
        hb_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // `interval` fires its first tick immediately; we already wrote
        // an initial heartbeat above, so swallow that fire.
        hb_ticker.tick().await;

        let mut rescan_ticker = tokio::time::interval(ROSTER_RESCAN_INTERVAL);
        rescan_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        rescan_ticker.tick().await;

        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                _ = &mut shutdown => {
                    tracing::info!("shutdown received; aborting project tasks (sessions remain alive)");
                    tasks.abort_all();
                    while tasks.join_next().await.is_some() {}
                    daemon::remove_heartbeat(&self.paths);
                    return Ok(());
                }
                _ = hb_ticker.tick() => {
                    if let Err(err) = daemon::write_heartbeat(&self.paths) {
                        tracing::warn!(?err, "heartbeat write failed");
                    }
                }
                _ = rescan_ticker.tick() => {
                    self.spawn_new_rostered_projects(&mut tasks, &mut spawned, "rescan");
                }
                Some(joined) = tasks.join_next() => {
                    match joined {
                        Ok((slug, Ok(()))) => {
                            tracing::info!(slug, "project event loop ended cleanly");
                        }
                        Ok((slug, Err(err))) => {
                            tracing::warn!(slug, error = ?err, "project event loop errored");
                        }
                        Err(je) if je.is_cancelled() => {}
                        Err(je) => {
                            tracing::warn!(error = ?je, "project task panicked");
                        }
                    }
                }
            }
        }
    }

    /// Walk the projects root and spawn event loops for slugs not yet
    /// in `spawned`. Shared between startup (`origin = "startup"`) and
    /// the periodic rescan tick (`origin = "rescan"`). Slugs without a
    /// `workflow.yaml` are skipped silently — those are legacy V0.3.x
    /// phase-driven projects with no event-loop equivalent.
    fn spawn_new_rostered_projects(
        self: &Arc<Self>,
        tasks: &mut JoinSet<(String, Result<()>)>,
        spawned: &mut HashSet<String>,
        origin: &'static str,
    ) {
        let projects = match queries::collect_projects(&self.paths) {
            Ok(p) => p,
            Err(err) => {
                tracing::warn!(?err, origin, "collect_projects failed; roster unchanged");
                return;
            }
        };
        for proj in projects {
            let slug = proj.state.slug.clone();
            if spawned.contains(&slug) {
                continue;
            }
            let project_dir = self.paths.project_dir(&slug);
            if !project_dir.join("workflow.yaml").exists()
                && !project_dir.join(".ccteam").join("workflow.yaml").exists()
            {
                tracing::debug!(slug, "no workflow.yaml; skipping (pre-V0.4.0 project)");
                continue;
            }
            if origin == "rescan" {
                tracing::info!(slug, "hot-loaded new project; starting event loop");
            } else {
                tracing::info!(slug, "starting project event loop");
            }
            let orch = Arc::clone(self);
            let slug_for_task = slug.clone();
            tasks.spawn(async move {
                let res = orch.run_project(&slug_for_task).await;
                (slug_for_task, res)
            });
            spawned.insert(slug);
        }
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
                    self.check_inbox(slug, spec, project_dir, progress_path).await;
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

    /// Default kicker prompt when neither marker nor artifact provides
    /// one. `claude --bg --agent <role>` requires a positional prompt;
    /// without it the session parks at "stuck on a startup dialog". The
    /// agent's `.claude/agents/<role>.md` body is the real instruction
    /// set — this string just nudges the LLM to start consulting it.
    const DEFAULT_KICK_PROMPT: &'static str =
        "Begin your assigned task. Your role definition is in .claude/agents/<your-role>.md.";

    async fn try_spawn(
        &self,
        slug: &str,
        role: &str,
        agent: &AgentSpec,
        project_dir: &std::path::Path,
        progress_path: &std::path::Path,
    ) -> Result<()> {
        self.try_spawn_with_prompt(slug, role, agent, project_dir, progress_path, None)
            .await
    }

    async fn try_spawn_with_prompt(
        &self,
        slug: &str,
        role: &str,
        agent: &AgentSpec,
        project_dir: &std::path::Path,
        progress_path: &std::path::Path,
        prompt: Option<String>,
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
        let kick = prompt.unwrap_or_else(|| Self::DEFAULT_KICK_PROMPT.to_string());
        let opts = SpawnOpts {
            harness: match agent.executor {
                Executor::Claude => "claude-code",
                Executor::Codex => "codex",
            },
            slug: slug.to_string(),
            sid: sid.clone(),
            cwd: project_dir.to_path_buf(),
            role: role.to_string(),
            extra_args: vec![kick],
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
                // V0.4.5 F80 — record `job_id` on the agent_spawn so
                // the read-side (queries::workflow_summary) and
                // poll_completions can cross-reference
                // `~/.claude/jobs/<id>/state.json` for liveness.
                // Codex sessions leave it null (no `--bg` surface yet).
                progress::append_event(
                    progress_path,
                    &json!({
                        "event": "agent_spawn",
                        "role": role,
                        "session_id": handle.sid,
                        "tmux_session": handle.tmux_session,
                        "job_id": handle.job_id,
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

        // V0.4.5 F80 — stale-spawn cleanup. Walk progress.jsonl for
        // any `agent_spawn` rows still missing a matching `agent_done`,
        // probe `~/.claude/jobs/<job_id>/state.json` to decide whether
        // they're really alive or were SIGKILL casualties from a prior
        // daemon shutdown. Phantom rows get a synthetic `agent_done`
        // here so the running count drops back to ground truth without
        // the user having to nuke `progress.jsonl`. The in-memory
        // `running` map is already correct (handles for dead sessions
        // never repopulated on daemon restart); this is purely about
        // the progress-jsonl event log.
        let events = progress::read_all_events(progress_path).unwrap_or_default();
        let in_memory_sids: std::collections::HashSet<String> = {
            let running = self.running.lock().await;
            running
                .values()
                .flat_map(|v| v.iter().map(|h| h.sid.clone()))
                .collect()
        };
        for (sid, job_id, role) in progress::open_agent_spawns(&events) {
            if in_memory_sids.contains(&sid) {
                continue; // genuinely tracked by this orchestrator instance
            }
            let verdict = crate::claude_job::probe_job(job_id.as_deref());
            let crate::claude_job::JobLiveness::Terminal { status, cost_usd } = verdict else {
                continue;
            };
            tracing::info!(
                slug,
                role = role.as_str(),
                session_id = sid.as_str(),
                ?job_id,
                status,
                cost_usd,
                "F80 stale-spawn cleanup: emitting synthetic agent_done",
            );
            finished.push((
                role,
                SessionHandle {
                    tmux_session: String::new(),
                    harness: "claude-code".to_string(),
                    sid,
                    job_id,
                    pid: None,
                    started_at: Utc::now(),
                },
                Some(cost_usd),
                status.to_string(),
            ));
        }

        for (role, handle, cost_usd, status) in finished {
            if let Some(c) = cost_usd {
                *self.cost_accum.lock().await += c;
                // V0.4.5 F80 — keep `state.cost_used_usd` (read by
                // `ccteam show <slug>` + the dashboard cost column)
                // in sync with the `agent_done` cost the web UI
                // already aggregates from progress.jsonl. Pre-F80
                // these two surfaces diverged: hooks updated
                // state.cost_used_usd from the per-tool cost feed,
                // but agent_done cost lived only in the event log.
                self.bump_project_state_cost(slug, c).await;
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

    /// V0.4.5 F80 — best-effort bump of `state.cost_used_usd` on disk
    /// so the dashboard cost column (sourced from `ProjectState`) and
    /// the web UI workflow card (sourced from progress.jsonl
    /// `agent_done.cost_usd`) report the same number. Read-modify-write
    /// is wrapped in `ProjectState::save` which is atomic + makes a
    /// `.bak`; a missing `state.json` (test fixtures, transient race)
    /// is silently skipped — the source-of-truth event log already has
    /// the cost. Errors log but never bubble; the orchestrator's
    /// dispatch loop must not stall on cost-bookkeeping.
    async fn bump_project_state_cost(&self, slug: &str, delta: f64) {
        if delta <= 0.0 {
            return;
        }
        let state_path = self.paths.project_state(slug);
        if !state_path.exists() {
            return;
        }
        let mut state = match crate::state::ProjectState::load(&state_path) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(slug, ?err, "F80 cost bump: load state.json failed");
                return;
            }
        };
        state.cost_used_usd += delta;
        if let Err(err) = state.save(&state_path) {
            tracing::warn!(slug, ?err, "F80 cost bump: save state.json failed");
        }
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
            let parsed = std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
            let role = match parsed
                .as_ref()
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
            // Optional `prompt` field (top-level or under `overrides`)
            // becomes the positional prompt for `claude --bg`. Falls
            // back to `Self::DEFAULT_KICK_PROMPT` when absent.
            let prompt = parsed.as_ref().and_then(|v| {
                v.get("prompt")
                    .and_then(|p| p.as_str())
                    .or_else(|| v.pointer("/overrides/prompt").and_then(|p| p.as_str()))
                    .map(|s| s.to_string())
            });
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
                .try_spawn_with_prompt(slug, &role, agent, project_dir, progress_path, prompt)
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

    /// Scan `<project>/.ccteam/inbox/msg-*.md` for unconsumed messages
    /// (written by `ccteam__send_to_session` / `ccteam__inject_decision`
    /// / channel adapters), archive each to `.ccteam/inbox.archived/`,
    /// emit one `inbox_received` event, and — when the message body is
    /// non-empty — auto-write a `.ccteam/spawn_requests/` marker so a
    /// fresh agent session picks up the message as its kick prompt.
    ///
    /// ## V0.4.0 delivery semantics
    ///
    /// V0.3.x send-keys'd inbox bodies into a long-lived tmux claude
    /// session; V0.4.0 `claude --bg` sessions are one-shot, so there's
    /// no idle inject target. Instead we spawn a fresh session with the
    /// message as its prompt — semantically equivalent to "user typed
    /// this message into a new chat with the agent."
    ///
    /// **Target role selection**:
    /// - Front-matter `target_role: <role>` → that role (must exist in
    ///   workflow.yaml; else log + skip the spawn but still archive).
    /// - Otherwise → first `Trigger::Manual` role in `spec.agents`.
    /// - No manual role exists → archive only + log a hint.
    ///
    /// **Opt-out**: front-matter `no_spawn: true` skips the auto-spawn
    /// (archive only, useful for note-taking / audit-only messages).
    ///
    /// **Parse-failed messages**: archive but never auto-spawn — the
    /// body could be anything, including stale binary garbage.
    async fn check_inbox(
        &self,
        slug: &str,
        spec: &WorkflowSpec,
        project_dir: &std::path::Path,
        progress_path: &std::path::Path,
    ) {
        let ccteam_dir = project_dir.join(".ccteam");
        let mailbox = SessionMailbox::for_ccteam_dir(&ccteam_dir);
        let files = match mailbox.list_inbox() {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!(slug, ?err, "inbox list failed");
                return;
            }
        };
        if files.is_empty() {
            return;
        }
        let archive_dir = ccteam_dir.join("inbox.archived");
        if let Err(err) = std::fs::create_dir_all(&archive_dir) {
            tracing::warn!(
                slug,
                ?err,
                "inbox.archived mkdir failed; messages stay in inbox"
            );
            return;
        }

        for path in files {
            let filename = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            let raw = match std::fs::read_to_string(&path) {
                Ok(s) => s,
                Err(err) => {
                    tracing::warn!(slug, ?err, filename, "inbox read failed; leaving in place");
                    continue;
                }
            };
            let parsed = InboxMessage::parse(&raw).ok();
            // Best-effort second pass: when the frontmatter has extra
            // routing fields (`target_role`, `no_spawn`) the strict
            // `InboxMessage` schema doesn't model, fall back to a loose
            // YAML probe so users can hint dispatch without breaking the
            // typed front-matter contract.
            let (loose_target, loose_no_spawn) = parse_inbox_routing_hints(&raw);
            let (source, source_user, body, body_summary, parse_failed) = match &parsed {
                Some(msg) => {
                    let summary: String = msg.body.chars().take(500).collect();
                    (
                        msg.front.source.clone(),
                        msg.front.source_user.clone(),
                        msg.body.clone(),
                        summary,
                        false,
                    )
                }
                None => {
                    let summary: String = raw.chars().take(500).collect();
                    (String::new(), String::new(), raw.clone(), summary, true)
                }
            };
            let archived_path = archive_dir.join(&filename);
            if let Err(err) = std::fs::rename(&path, &archived_path) {
                tracing::warn!(
                    slug,
                    ?err,
                    filename,
                    "inbox archive rename failed; will retry next tick"
                );
                continue;
            }

            // Decide spawn target — only for parsed messages with
            // non-empty body and no explicit opt-out.
            let body_trim = body.trim();
            let auto_spawn_role: Option<String> =
                if parse_failed || body_trim.is_empty() || loose_no_spawn {
                    None
                } else if let Some(target) = loose_target {
                    if spec.agents.contains_key(&target) {
                        Some(target)
                    } else {
                        tracing::warn!(
                            slug,
                            target,
                            "inbox target_role not in workflow.yaml; archive only"
                        );
                        None
                    }
                } else {
                    spec.agents
                        .iter()
                        .find(|(_, a)| matches!(a.trigger, Trigger::Manual))
                        .map(|(r, _)| r.clone())
                };

            let mut spawn_marker: Option<String> = None;
            if let Some(role) = &auto_spawn_role {
                let marker_dir = ccteam_dir.join("spawn_requests");
                if std::fs::create_dir_all(&marker_dir).is_ok() {
                    let marker_name = format!(
                        "{}-inbox-{}.json",
                        role,
                        Utc::now().format("%Y%m%dT%H%M%S%f")
                    );
                    let marker_path = marker_dir.join(&marker_name);
                    let payload = json!({
                        "role": role,
                        "prompt": body_trim,
                        "source": "inbox",
                        "source_filename": filename,
                        "requested_at": Utc::now().to_rfc3339(),
                    });
                    if let Err(err) = std::fs::write(
                        &marker_path,
                        serde_json::to_string_pretty(&payload).unwrap_or_default(),
                    ) {
                        tracing::warn!(slug, ?err, role, "inbox auto-spawn marker write failed");
                    } else {
                        spawn_marker = Some(marker_name);
                    }
                }
            }

            let _ = progress::append_event(
                progress_path,
                &json!({
                    "event": "inbox_received",
                    "slug": slug,
                    "filename": filename,
                    "source": source,
                    "source_user": source_user,
                    "body_summary": body_summary,
                    "parse_failed": parse_failed,
                    "archived_path": archived_path.to_string_lossy(),
                    "auto_spawn_role": auto_spawn_role,
                    "auto_spawn_marker": spawn_marker,
                    "ts": Utc::now().to_rfc3339(),
                }),
            );
            tracing::info!(
                slug,
                filename,
                source = %source,
                spawn = ?auto_spawn_role,
                "inbox message archived + routed"
            );
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
        // Real `claude --bg` 2.1.x writes `state` ∈ {working, done,
        // failed, crashed}; legacy / fixture rows may carry `status` ∈
        // {running, completed, error, stopped}. Read both, normalize
        // to the SessionStatus::Done payload tag the orchestrator's
        // downstream events expect.
        let raw = v
            .get("state")
            .and_then(|s| s.as_str())
            .or_else(|| v.get("status").and_then(|s| s.as_str()))
            .unwrap_or("working");
        let normalized = match raw {
            "done" | "completed" => Some("completed"),
            "failed" | "crashed" | "error" => Some("error"),
            "stopped" => Some("stopped"),
            // "working" / "running" / "idle" / "active" / unknown → still running
            _ => None,
        };
        let Some(status) = normalized else {
            return SessionStatus::Running;
        };
        let cost_usd = v
            .get("cost_usd")
            .and_then(|n| n.as_f64())
            .or_else(|| v.get("cost").and_then(|n| n.as_f64()));
        SessionStatus::Done {
            cost_usd,
            status: status.to_string(),
        }
    }

    /// State.json path:
    /// - codex → `~/.ccteam/codex/<sid>/state.json`
    /// - claude-code → `~/.claude/jobs/<job_id>/state.json` (where
    ///   `job_id` is the `daemonShort` from the `backgrounded · <id>`
    ///   spawn line)
    /// - test override via `CCTEAM_SESSION_STATE_DIR` env keys by `sid`
    ///   so existing fixture stubs (write `<dir>/<sid>/state.json`)
    ///   keep working without job_id plumbing.
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
        let id = handle.job_id.as_deref().unwrap_or("__no_job_id__");
        home.join(".claude")
            .join("jobs")
            .join(id)
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

    pub async fn test_check_inbox(
        &self,
        slug: &str,
        spec: &WorkflowSpec,
        project_dir: &std::path::Path,
        progress_path: &std::path::Path,
    ) {
        self.check_inbox(slug, spec, project_dir, progress_path)
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
