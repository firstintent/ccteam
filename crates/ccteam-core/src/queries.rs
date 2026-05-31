//! V0.3 M5.1 — read-side query helpers shared by every channel layer.
//!
//! Promoted from `ccteam-cli/src/commands.rs` (where they lived as
//! `pub fn`s but were not callable from sibling crates because
//! depending on the binary `ccteam-cli` is a dep-graph anti-pattern).
//! Mirrors `actions.rs` (the M5.0 write-helper promotion):
//!
//! - the V0.3 web UI crate (`ccteam-web`) reads project state /
//!   progress events through this module without depending on
//!   `ccteam-cli`.
//! - the MCP server in `ccteam-cli::mcp_serve` consumes these helpers
//!   identically (the function bodies are unchanged from their
//!   `commands.rs` originals; only their home moves).
//! - `commands.rs::run_ls` / `run_progress` re-export the names from
//!   here so existing callers keep their current `use` lines minus the
//!   module path change.
//!
//! These helpers are **read-only**:
//!
//! - they do **not** mutate `state.json` or write progress events.
//! - they do **not** parse tmux output (architecture red line,
//!   CLAUDE.md §三 — `progress.jsonl` is the orchestrator's SoT).
//! - corrupt / unparseable files surface as logged warnings + skipped
//!   entries; never panics or crashes the caller.
//!
//! Architecture refs: `docs/versions/v0-3/prd.md` §4 (M5.1 dashboard data
//! source), `docs/dev-coupling-audit.md` F45 (extends the M5.0
//! write-helper promotion to the read side), `docs/tech-design.md`
//! §5.5 progress.jsonl SoT.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Timelike, Utc};
use serde::Serialize;
use serde_json::Value;

use crate::paths::CcteamPaths;
use crate::progress::{
    self, current_agent_sessions_with_liveness, escalation_count, open_agent_spawns,
    workflow_cost_total, AgentSessionStatus, AgentSessionSummary,
};
use crate::state::ProjectState;
use crate::team::TeamKind;
use crate::workflow::{Trigger, WorkflowError, WorkflowSpec};

/// Project metadata with derived fields used by `ccteam ls`, the MCP
/// `ls` tool, and the V0.3 web dashboard. Pulled out so each renderer
/// (text / JSON / HTML) shares one source of truth instead of
/// re-deriving `age_seconds` / `stall_silent_seconds` per call site.
#[derive(Debug)]
pub struct ProjectSummary {
    pub state: ProjectState,
    pub age_seconds: u64,
    pub stall_silent_seconds: u64,
}

/// Enumerate projects under ccteam management.
///
/// V0.4.2 F73: `~/.ccteam/config.yaml::projects[]` is the canonical
/// source. Each entry's `state.json` is loaded from its absolute
/// `path` (which may live outside `paths.projects_root` — adopted
/// repos in `~/code/...` etc.).
///
/// **Legacy fallback**: for slugs not yet registered, also walk
/// `paths.projects_root` and include any directory whose
/// `.ccteam/state.json` parses. This keeps V0.4.1 installs working
/// until `ccteam doctor --migrate-v041-to-v042` (F74) folds them
/// into config.yaml. After migration the walk finds nothing new and
/// becomes a no-op.
///
/// Skips entries that lack `state.json` or whose `state.json` fails
/// to parse — those get a warn-level log line but do not abort the
/// walk. Slug ordering is stable (sorted) so renderers don't need
/// to re-sort.
pub fn collect_projects(paths: &CcteamPaths) -> Result<Vec<ProjectSummary>> {
    let mut out = Vec::new();
    let mut seen_slugs: std::collections::HashSet<String> = std::collections::HashSet::new();

    // 1. config.yaml::projects[] is the canonical SoT (V0.4.2 F73).
    let cfg = crate::config::load(&paths.root).unwrap_or_else(|err| {
        tracing::warn!(?err, "load config.yaml failed; treating registry as empty");
        crate::config::CcteamConfig::default()
    });
    for entry in &cfg.projects {
        let state_path = entry.path.join(".ccteam").join("state.json");
        if !state_path.exists() {
            tracing::warn!(
                slug = %entry.slug,
                path = %entry.path.display(),
                "registered project's state.json is missing; skipping (run `ccteam abandon {}` to clean up)",
                entry.slug,
            );
            continue;
        }
        let state = match ProjectState::load(&state_path) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(
                    slug = %entry.slug,
                    error = %err,
                    "skip registered project: state.json load failed",
                );
                continue;
            }
        };
        seen_slugs.insert(state.slug.clone());
        out.push(summary_from_state(state));
    }

    // 2. Legacy fallback: walk projects_root for unregistered slugs.
    let dir = &paths.projects_root;
    if dir.exists() {
        for entry in
            std::fs::read_dir(dir).with_context(|| format!("read_dir {}", dir.display()))?
        {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let Some(slug) = entry.file_name().to_str().map(String::from) else {
                continue;
            };
            if seen_slugs.contains(&slug) {
                continue;
            }
            let state_path = paths.project_state(&slug);
            if !state_path.exists() {
                continue;
            }
            let state = match ProjectState::load(&state_path) {
                Ok(s) => s,
                Err(err) => {
                    tracing::warn!(slug, error = %err, "skip project: state.json failed to load");
                    continue;
                }
            };
            out.push(summary_from_state(state));
        }
    }

    out.sort_by(|a, b| a.state.slug.cmp(&b.state.slug));
    Ok(out)
}

fn summary_from_state(state: ProjectState) -> ProjectSummary {
    let now = Utc::now();
    let age = now
        .signed_duration_since(state.created_at)
        .num_seconds()
        .max(0) as u64;
    let silent = state
        .last_progress_event_at
        .map(|t| now.signed_duration_since(t).num_seconds().max(0) as u64)
        .unwrap_or(age);
    ProjectSummary {
        state,
        age_seconds: age,
        stall_silent_seconds: silent,
    }
}

/// Tail the last `n` JSON-Lines events for a project.
///
/// Workflow / multi-workflow projects read the legacy flat
/// `~/.ccteam/progress/<slug>.jsonl` file. Flex projects read every
/// `~/.ccteam/progress/<slug>/<sid>.jsonl` stream and merge them by
/// their best-effort `ts` field so dashboard / CLI readers keep a
/// project-level view without forcing the orchestrator to inspect
/// harness snapshots.
pub fn collect_recent_events(paths: &CcteamPaths, slug: &str, n: usize) -> Result<Vec<Value>> {
    let state = ProjectState::load(&paths.project_state(slug)).ok();
    if state
        .as_ref()
        .is_some_and(|s| s.team_kind == TeamKind::Flex)
    {
        return collect_recent_flex_events(paths, slug, n);
    }

    let path = paths.progress_jsonl(slug);
    read_tail_events(&path, n)
}

fn collect_recent_flex_events(paths: &CcteamPaths, slug: &str, n: usize) -> Result<Vec<Value>> {
    let dir = paths.progress_dir().join(slug);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut all = Vec::new();
    for entry in std::fs::read_dir(&dir).with_context(|| format!("read_dir {}", dir.display()))? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        all.extend(read_tail_events(&path, n)?);
    }
    all.sort_by_key(event_sort_key);
    if all.len() > n {
        let drop = all.len() - n;
        all.drain(..drop);
    }
    Ok(all)
}

fn read_tail_events(path: &std::path::Path, n: usize) -> Result<Vec<Value>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let body = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let mut all: Vec<Value> = body
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    if all.len() > n {
        let drop = all.len() - n;
        all.drain(..drop);
    }
    Ok(all)
}

fn event_sort_key(event: &Value) -> String {
    event
        .get("ts")
        .and_then(|ts| ts.as_str())
        .unwrap_or("")
        .to_string()
}

// ---------------- V0.4.0 F67 WorkflowSummary ----------------

/// Per-agent aggregate the workflow view (F68 SPA) renders. Derived
/// from progress.jsonl events + the project's `workflow.yaml` agent
/// dir convention. `queued_count` stays `0` in V0.4.0 — F66's
/// `pending` queue is in-memory and not yet persisted to disk; once
/// F67/F68 wire a pending file it surfaces here.
#[derive(Debug, Clone, Serialize)]
pub struct AgentStatus {
    /// Agent role (key in `WorkflowSpec::agents`).
    pub role: String,
    /// Number of `agent_spawn` events for this role with no matching
    /// terminal `agent_done`.
    pub running_count: u32,
    /// Always `0` in V0.4.0. F66's pending queue is in-memory; a
    /// later PR may persist it and populate this field.
    pub queued_count: u32,
    /// Sum of `cost_usd` across every terminal `agent_done` event
    /// for this role.
    pub total_cost_usd: f64,
    /// Status of the most recently terminated session for this role
    /// (by `started_at`), or `None` when no `agent_done` has fired
    /// yet for this role.
    pub last_session_status: Option<AgentSessionStatus>,
}

/// V0.4.6 F91 — cost aggregation surface. SoT is `progress.jsonl::agent_done`
/// for historical totals and `~/.claude/jobs/<id>/state.json::cost_usd_total`
/// (read live) for the active sessions.
///
/// Pre-F91 ccteam maintained `ProjectState::cost_used_usd` via the
/// `cost-accumulate` PostToolUse hook + the F80 orchestrator bump on
/// synthetic `agent_done`. Both paths were wedge-prone: hook misses,
/// `claude --bg` argv drift, or daemon SIGKILL casualties left the
/// number stale or low. F91 retires that accumulator entirely; the new
/// source of truth is the per-event cost Claude itself reports, surfaced
/// through this struct.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct CostSummary {
    /// Sum of `cost_usd` across every `agent_done` event with `ts`
    /// inside the last 24h (relative to wall-clock at the call). Events
    /// with missing / unparseable `ts` are folded into the 24h bucket so
    /// recent rows don't silently disappear when timestamps are absent.
    pub cost_24h_usd: f64,
    /// Sum of `cost_usd_total` (falling back to `cost_usd`) read live
    /// from each currently-open agent session's
    /// `~/.claude/jobs/<job_id>/state.json`. Missing files / unparseable
    /// JSON / missing fields contribute 0.0 (no failure mode is fatal —
    /// stale rows just under-report).
    pub cost_active_usd: f64,
    /// Sum of `cost_usd` across every `agent_done` event in the slice
    /// (i.e. lifetime total recorded in this project's progress.jsonl).
    /// Drives the "lifetime" headline + budget overruns that look beyond
    /// the 24h window.
    pub cost_total_usd: f64,
    /// Number of `agent_done` events folded into [`cost_24h_usd`].
    pub session_count_24h: u32,
    /// Number of open `agent_spawn` events (no matching `agent_done`)
    /// whose [`cost_active_usd`] contribution was probed.
    pub session_count_active: u32,
    /// V0.6.0 Wave 3 F112 — per-vendor breakdown of the 24h cost
    /// bucket. Keys are JSON-lowercased vendor names (`"claude"`,
    /// `"codex"`). The aggregate `cost_24h_usd` remains the sum across
    /// all vendors (consumers tolerant of the legacy single-vendor
    /// shape see no diff). Empty when no `agent_done` event in the
    /// 24h window carried a `vendor` field — V0.5.x events lacking
    /// `vendor` are silently folded into `cost_24h_usd` only.
    #[serde(default)]
    pub cost_24h_by_vendor: std::collections::BTreeMap<String, f64>,
    /// V0.6.0 Wave 3 F112 — per-vendor lifetime cost. Same vendor-key
    /// semantics as `cost_24h_by_vendor`. Drives the
    /// `ccteam-control show-cost` per-vendor breakdown and the
    /// per-vendor budget cap check.
    #[serde(default)]
    pub cost_total_by_vendor: std::collections::BTreeMap<String, f64>,
}

/// Build a [`CostSummary`] for `slug` by reading `progress_path`
/// (typically `paths.progress_jsonl(slug)`) and probing each open
/// agent session's `~/.claude/jobs/<id>/state.json` for live cost.
///
/// `progress_path` is taken explicitly (instead of derived from
/// `paths + slug`) so future flex-project callers can sum across
/// per-sid streams without forcing this helper to know the team kind.
/// For workflow projects pass `&paths.progress_jsonl(slug)` directly.
///
/// **Side-effect-free.** Reads `progress_path` once + one stat/read per
/// open job_id. No mutation to state.json (per F91 — that path is
/// being retired). Returns `Ok(default())` when progress.jsonl is
/// missing rather than erroring; callers (CLI / SPA / budget cap) want
/// a zeroed surface for fresh projects.
pub fn cost_summary(slug: &str, progress_path: &Path, paths: &CcteamPaths) -> Result<CostSummary> {
    // Tolerate missing files: a fresh project's progress.jsonl doesn't
    // exist yet and that must surface as zeroed cost, not an error
    // propagated up through `workflow_summary` / `ccteam show`.
    let _ = (slug, paths); // slug/paths reserved for flex-project routing later.
    let events = progress::read_all_events(progress_path).unwrap_or_default();
    Ok(compute_cost_summary(&events, Utc::now(), |job_id| {
        crate::claude_job::probe_job(job_id)
    }))
}

/// Pure, IO-free core of [`cost_summary`]. Takes the parsed event slice,
/// a wall-clock `now` for the 24h window, and a `probe` closure that
/// resolves each open `job_id` to a `JobLiveness` so the helper can
/// total live cost without depending on the filesystem.
///
/// Exposed so unit tests can drive both halves (event slice + probe
/// outcome) deterministically. Production callers route through
/// [`cost_summary`] which wires the closure to
/// [`crate::claude_job::probe_job`].
pub fn compute_cost_summary<F>(events: &[Value], now: DateTime<Utc>, probe: F) -> CostSummary
where
    F: Fn(Option<&str>) -> crate::claude_job::JobLiveness,
{
    let cutoff_24h = now - Duration::hours(24);

    let mut cost_total_usd = 0.0;
    let mut cost_24h_usd = 0.0;
    let mut session_count_24h: u32 = 0;
    let mut cost_24h_by_vendor: std::collections::BTreeMap<String, f64> =
        std::collections::BTreeMap::new();
    let mut cost_total_by_vendor: std::collections::BTreeMap<String, f64> =
        std::collections::BTreeMap::new();
    for event in events {
        if event.get("event").and_then(|s| s.as_str()) != Some("agent_done") {
            continue;
        }
        let cost = event
            .get("cost_usd")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        cost_total_usd += cost;
        // V0.6.0 Wave 3 F112: optional `vendor` field on `agent_done`.
        // V0.5.x events lack this field; they contribute to the
        // aggregate cost_total_usd / cost_24h_usd but not the per-
        // vendor breakdown (the F84 per-vendor budget caps therefore
        // only act on V0.6+ events, matching the per-vendor opt-in
        // shape of `Budgets { claude, codex }`).
        let vendor = event.get("vendor").and_then(|v| v.as_str());
        if let Some(v) = vendor {
            *cost_total_by_vendor.entry(v.to_string()).or_insert(0.0) += cost;
        }

        // 24h filter: events with missing or unparseable `ts` are
        // counted in the 24h bucket (defensive — newly-written rows
        // sometimes lack ts during transitional schemas; folding them
        // in matches the "recent" intuition the dashboard / budget cap
        // wants).
        let in_window = match event.get("ts").and_then(|s| s.as_str()) {
            Some(ts) => match DateTime::parse_from_rfc3339(ts) {
                Ok(parsed) => parsed.with_timezone(&Utc) >= cutoff_24h,
                Err(_) => true,
            },
            None => true,
        };
        if in_window {
            cost_24h_usd += cost;
            session_count_24h = session_count_24h.saturating_add(1);
            if let Some(v) = vendor {
                *cost_24h_by_vendor.entry(v.to_string()).or_insert(0.0) += cost;
            }
        }
    }

    // Active cost: probe each open agent_spawn's job_id. `Running`
    // verdicts route through `transcript_scanner::session_cost_from_jsonl`
    // because state.json::cost_usd_total reads `0` on the host (V0.4.6
    // probe). `Terminal` verdicts carry the same transcript-derived cost
    // (claude_job::classify now does the fallback internally) so we can
    // surface it for stale sessions before the synthetic agent_done is
    // written.
    let open = open_agent_spawns(events);
    let mut cost_active_usd = 0.0;
    let session_count_active = open.len() as u32;
    for (_sid, job_id, _role) in open {
        match probe(job_id.as_deref()) {
            crate::claude_job::JobLiveness::Running => {
                if let Some(id) = job_id.as_deref() {
                    let path = crate::claude_job::job_state_path(id);
                    if let Ok(raw) = std::fs::read_to_string(&path) {
                        if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                            cost_active_usd += crate::claude_job::resolve_cost_usd(&v);
                        }
                    }
                }
            }
            crate::claude_job::JobLiveness::Terminal {
                status: _,
                cost_usd,
            } => {
                cost_active_usd += cost_usd;
            }
        }
    }

    CostSummary {
        cost_24h_usd,
        cost_active_usd,
        cost_total_usd,
        session_count_24h,
        session_count_active,
        cost_24h_by_vendor,
        cost_total_by_vendor,
    }
}

/// Snapshot of one project's workflow state for the meta-agent / web
/// dashboard. Cheap to compute (`O(N)` over progress events + one
/// `read_dir` per agent's artifact directory) so callers can refresh
/// at SPA poll rates without instrumentation.
///
/// Output ordering: `agents` is sorted by role name ASCII; consumers
/// that need the YAML declaration order can re-sort against the spec.
#[derive(Debug, Clone, Serialize)]
pub struct WorkflowSummary {
    /// `WorkflowSpec::name` for the project, or `""` when the project
    /// has no workflow.yaml (e.g. legacy V0.3.x slug discovered by
    /// `collect_projects` before migration).
    pub workflow_name: String,
    /// One entry per `WorkflowSpec::agents` role (sorted ASCII).
    pub agents: Vec<AgentStatus>,
    /// `<input or output dir relative path>` → file count. Each
    /// agent's `input` AND `output` directories are stat-ed (if set
    /// in workflow.yaml). Missing dirs map to `0`.
    pub artifact_counts: HashMap<String, u64>,
    /// Sum of cost across every `agent_done` event in the slice. Kept
    /// for SPA back-compat (mirrors `cost.cost_total_usd`); F90 will
    /// transition the dashboard to read `cost.cost_24h_usd` directly.
    pub total_cost_usd: f64,
    /// V0.4.6 F91 — cost SoT surface used by `ccteam show` + F84 budget
    /// cap + F90 sparkline. Lives alongside `total_cost_usd` until F90
    /// finishes the SPA cutover; both fields will report consistent
    /// totals during the transition.
    pub cost: CostSummary,
    /// Count of `escalation` events in the slice.
    pub escalation_count: u32,
    /// `role` → `"waiting"` / `"released"` / `"fired"`. Derived from
    /// `gate_triggered` events: any role that appears in a
    /// `gate_triggered` event is `"fired"`; remaining `Trigger::Gate`
    /// roles in the spec stay `"waiting"`.
    pub gate_states: HashMap<String, String>,
}

impl Default for WorkflowSummary {
    fn default() -> Self {
        Self {
            workflow_name: String::new(),
            agents: Vec::new(),
            artifact_counts: HashMap::new(),
            total_cost_usd: 0.0,
            cost: CostSummary::default(),
            escalation_count: 0,
            gate_states: HashMap::new(),
        }
    }
}

/// Build a [`WorkflowSummary`] for `slug` by reading
/// `<project>/workflow.yaml` (or `<project>/.ccteam/workflow.yaml`)
/// and merging with the project's progress.jsonl event stream.
///
/// Returns `Ok(WorkflowSummary::default())` (with `workflow_name = ""`)
/// when the project has no workflow.yaml — this lets the SPA show a
/// blank workflow panel for legacy / pre-V0.4.0 projects without 500-ing.
///
/// Errors only on hard IO failure (e.g. `state.json` unreadable mid-read,
/// project directory absent).
pub fn workflow_summary(slug: &str, paths: &CcteamPaths) -> Result<WorkflowSummary> {
    let project_dir = paths.project_dir(slug);

    // Try to load workflow.yaml; absence is non-fatal (legacy project).
    let spec = match WorkflowSpec::load_for_project(&project_dir) {
        Ok(s) => Some(s),
        Err(WorkflowError::NotFound(_)) => None,
        Err(err) => {
            tracing::warn!(
                slug,
                error = %err,
                "workflow.yaml present but failed to parse; returning empty summary",
            );
            None
        }
    };

    // Load progress events. Flex projects use sharded per-sid files
    // (read via `collect_recent_flex_events`); workflow uses the flat
    // `<slug>.jsonl`. F66 writes to the flat file for workflow
    // projects (where V0.4.0 lives), so we read that path
    // directly; flex stays consistent via `collect_recent_events`.
    let state = ProjectState::load(&paths.project_state(slug)).ok();
    let events: Vec<Value> = if state
        .as_ref()
        .is_some_and(|s| s.team_kind == TeamKind::Flex)
    {
        collect_recent_flex_events(paths, slug, usize::MAX).unwrap_or_default()
    } else {
        progress::read_all_events(&paths.progress_jsonl(slug)).unwrap_or_default()
    };

    let total_cost_usd = workflow_cost_total(&events);
    // V0.4.6 F91 — rich cost surface (24h / active / total). `cost`
    // shares the same agent_done aggregation as `total_cost_usd`; the
    // extra dimensions (24h window + live state.json probe) are what
    // F84 budget cap + F90 sparkline consume. `total_cost_usd` stays
    // for SPA back-compat until F90 finishes the cutover.
    let cost = compute_cost_summary(&events, Utc::now(), |job_id| {
        crate::claude_job::probe_job(job_id)
    });
    let escalation_count = escalation_count(&events);
    // V0.4.5 F80 — liveness-aware accounting. Each open `agent_spawn`
    // is cross-referenced against `~/.claude/jobs/<job_id>/state.json`
    // so phantom rows (daemon SIGKILL casualties whose process died
    // without writing `agent_done`) drop out of the running count
    // immediately, before the orchestrator's next `poll_completions`
    // tick writes the synthetic cleanup event.
    let sessions = current_agent_sessions_with_liveness(&events, |job_id| {
        crate::claude_job::probe_job(job_id)
    });

    let mut artifact_counts: HashMap<String, u64> = HashMap::new();
    let mut gate_states: HashMap<String, String> = HashMap::new();

    if let Some(spec) = &spec {
        // gate_states default to "waiting" for every Gate role; flip
        // to "fired" when a `gate_triggered` event names the role.
        for (role, agent) in &spec.agents {
            if matches!(agent.trigger, Trigger::Gate) {
                gate_states.insert(role.clone(), "waiting".to_string());
            }
        }
        for event in &events {
            if event.get("event").and_then(|s| s.as_str()) == Some("gate_triggered") {
                if let Some(role) = event.get("role").and_then(|s| s.as_str()) {
                    gate_states.insert(role.to_string(), "fired".to_string());
                }
            }
        }

        // Stat each agent's input + output dirs.
        for agent in spec.agents.values() {
            for rel in [agent.input.as_ref(), agent.output.as_ref()]
                .into_iter()
                .flatten()
            {
                let key = rel.display().to_string();
                let dir = project_dir.join(rel);
                let count = count_files_in_dir(&dir);
                artifact_counts.insert(key, count);
            }
        }
    }

    // Aggregate per-role stats from the session list.
    let agents = if let Some(spec) = &spec {
        let mut by_role: HashMap<&str, AgentStatus> = HashMap::new();
        for role in spec.agents.keys() {
            by_role.insert(
                role.as_str(),
                AgentStatus {
                    role: role.clone(),
                    running_count: 0,
                    queued_count: 0,
                    total_cost_usd: 0.0,
                    last_session_status: None,
                },
            );
        }
        // Walk sessions; sorted by `started_at` ascending so the
        // last entry per role is the most recently spawned.
        let mut last_by_role: HashMap<&str, &AgentSessionSummary> = HashMap::new();
        for session in &sessions {
            let Some(status) = by_role.get_mut(session.role.as_str()) else {
                // session.role not in current workflow.yaml — orphan from a
                // rename / removal. Skip the agent-card grid entry (the
                // historical events still surface in Events Timeline);
                // `workflow_cost_total` already aggregated the cost from
                // the raw event stream.
                continue;
            };
            accumulate_session(status, session);
            last_by_role.insert(session.role.as_str(), session);
        }
        for (role, last) in last_by_role {
            if let Some(status) = by_role.get_mut(role) {
                if !matches!(last.status, AgentSessionStatus::Running) {
                    status.last_session_status = Some(last.status.clone());
                }
            }
        }
        let mut out: Vec<AgentStatus> = by_role.into_values().collect();
        out.sort_by(|a, b| a.role.cmp(&b.role));
        out
    } else {
        Vec::new()
    };

    Ok(WorkflowSummary {
        workflow_name: spec.as_ref().map(|s| s.name.clone()).unwrap_or_default(),
        agents,
        artifact_counts,
        total_cost_usd,
        cost,
        escalation_count,
        gate_states,
    })
}

fn accumulate_session(status: &mut AgentStatus, session: &AgentSessionSummary) {
    match &session.status {
        AgentSessionStatus::Running => {
            status.running_count = status.running_count.saturating_add(1);
        }
        AgentSessionStatus::Done { cost_usd } | AgentSessionStatus::Errored { cost_usd } => {
            status.total_cost_usd += cost_usd;
        }
    }
}

// ---------------- V0.4.6 F91 cost SoT (F84 stub) ----------------

/// Rolling cost roll-up surfaced to `ccteam show` + the F84 budget
/// guard. **F84 stub**: this version only computes `cost_24h_usd` /
/// `cost_total_usd` / `session_count_24h` from progress.jsonl, which
/// is all F84's `enforce_budget` needs. F91's full impl (parallel
/// worktree) extends with `cost_active_usd` / `session_count_active`
/// by probing `~/.claude/jobs/<job_id>/state.json` for live sessions.
///
/// The fields that F84 doesn't read still ship here so the type
/// signature already matches the final F91 contract; F91 will only
/// touch the computation, not the shape. F84 unit tests assert
/// directly on `cost_24h_usd` so they keep passing after F91 merge.
/// V0.4.6 F84 — pure-event-slice helper derived from F91's
/// [`compute_cost_summary`]. F84 budget enforcement reads progress
/// events directly (no state.json probe) so we wrap the canonical
/// helper with a stub probe that classifies every job as terminal-zero.
/// This keeps F84 deterministic in unit tests while sharing the same
/// 24h window + `cost_total_usd` logic F91 already validated.
pub fn cost_summary_from_events(events: &[Value]) -> Result<CostSummary> {
    Ok(compute_cost_summary(events, Utc::now(), |_| {
        crate::claude_job::JobLiveness::Terminal {
            status: "completed",
            cost_usd: 0.0,
        }
    }))
}

/// V0.4.6 F84 — count `agent_spawn` events within `window` of now.
/// Used by the spawn-rate budget cap. Events with missing /
/// unparseable `ts` count as "recent" (defensive: prefer false
/// positive trip over silent overrun).
pub fn count_agent_spawns_within(events: &[Value], window: chrono::Duration) -> u32 {
    let cutoff = Utc::now() - window;
    let mut n = 0_u32;
    for evt in events {
        if evt.get("event").and_then(|s| s.as_str()) != Some("agent_spawn") {
            continue;
        }
        let ts_raw = evt.get("ts").and_then(|s| s.as_str()).unwrap_or("");
        let in_window = chrono::DateTime::parse_from_rfc3339(ts_raw)
            .map(|dt| dt.with_timezone(&Utc) >= cutoff)
            .unwrap_or(true);
        if in_window {
            n = n.saturating_add(1);
        }
    }
    n
}

fn count_files_in_dir(dir: &std::path::Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    rd.flatten()
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .count() as u64
}

// ---------------- V0.4.6 F90 — WorkflowView panel helpers ----------------

/// One watch-path entry surfaced by `GET
/// /api/v1/projects/<slug>/artifact_queue`. Snapshot of "what files
/// are sitting on this trigger path right now"; the SPA renders one
/// row per entry with count + oldest age + freshest filename.
///
/// `path` is the workflow.yaml-relative watch path (e.g.
/// `.ccteam/explore-requests/`). `oldest_age_seconds` and
/// `newest_filename` are best-effort — set to `None` when the dir is
/// empty or unreadable.
#[derive(Debug, Clone, Serialize)]
pub struct ArtifactQueueEntry {
    /// Workflow.yaml-relative watch path (preserved verbatim from
    /// `Trigger::Watch(<path>)`).
    pub path: String,
    /// Owning agent role (`workflow.yaml::agents.<role>` whose
    /// trigger declares this watch). Multiple roles may share one
    /// path; we list the first lexicographically and surface the
    /// rest in `extra_roles` so the SPA can label correctly.
    pub role: String,
    /// Number of files currently in the dir. Hidden dotfiles included
    /// (matches inotify's "new file" semantics).
    pub file_count: u64,
    /// Age of the oldest file in the dir, in seconds (best-effort
    /// — derived from `mtime`, falls back to `None` when unreadable).
    pub oldest_age_seconds: Option<u64>,
    /// Basename of the newest file (most recently modified) in the
    /// dir; `None` when the dir is empty.
    pub newest_filename: Option<String>,
}

/// V0.4.6 F90 — enumerate every `Trigger::Watch(<path>)` in a project's
/// workflow.yaml and stat each path against the filesystem. Read-only;
/// no progress events written. Used by the WorkflowView's "Artifact
/// Queue" panel.
///
/// Returns `Ok(vec![])` when the project has no workflow.yaml or no
/// `Trigger::Watch` agents — UI shows an empty queue panel rather than
/// 500-ing. Hard IO errors on individual dirs become `file_count=0`
/// rather than aborting the whole call (one broken dir shouldn't blank
/// the whole panel).
///
/// Output ordering: ASCII-sorted by `path`, then `role` as tiebreaker.
pub fn artifact_queue(slug: &str, paths: &CcteamPaths) -> Result<Vec<ArtifactQueueEntry>> {
    let project_dir = paths.project_dir(slug);
    let spec = match WorkflowSpec::load_for_project(&project_dir) {
        Ok(s) => s,
        Err(WorkflowError::NotFound(_)) => return Ok(Vec::new()),
        Err(err) => {
            tracing::warn!(
                slug,
                error = %err,
                "artifact_queue: workflow.yaml parse failed; returning empty queue"
            );
            return Ok(Vec::new());
        }
    };

    let mut entries: Vec<ArtifactQueueEntry> = Vec::new();
    for (role, agent) in &spec.agents {
        if let Trigger::Watch(rel) = &agent.trigger {
            let path_display = rel.display().to_string();
            let dir = project_dir.join(rel);
            let stat = stat_artifact_queue(&dir);
            entries.push(ArtifactQueueEntry {
                path: path_display,
                role: role.clone(),
                file_count: stat.file_count,
                oldest_age_seconds: stat.oldest_age_seconds,
                newest_filename: stat.newest_filename,
            });
        }
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.role.cmp(&b.role)));
    Ok(entries)
}

struct ArtifactQueueStat {
    file_count: u64,
    oldest_age_seconds: Option<u64>,
    newest_filename: Option<String>,
}

fn stat_artifact_queue(dir: &std::path::Path) -> ArtifactQueueStat {
    let now = std::time::SystemTime::now();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return ArtifactQueueStat {
            file_count: 0,
            oldest_age_seconds: None,
            newest_filename: None,
        };
    };

    let mut files: Vec<(std::path::PathBuf, std::time::SystemTime)> = Vec::new();
    for entry in rd.flatten() {
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        files.push((entry.path(), mtime));
    }

    if files.is_empty() {
        return ArtifactQueueStat {
            file_count: 0,
            oldest_age_seconds: None,
            newest_filename: None,
        };
    }

    let oldest_mtime = files.iter().map(|(_, m)| *m).min().unwrap();
    let oldest_age_seconds = now.duration_since(oldest_mtime).ok().map(|d| d.as_secs());

    let (newest_path, _) = files.iter().max_by_key(|(_, m)| *m).unwrap();
    let newest_filename = newest_path
        .file_name()
        .and_then(|s| s.to_str())
        .map(String::from);

    ArtifactQueueStat {
        file_count: files.len() as u64,
        oldest_age_seconds,
        newest_filename,
    }
}

// ---------------- artifact status (issue/PR/backlog count panel) ----------------

/// One artifact directory's status-grouped counts. Project-agnostic:
/// just groups `*.json` files in `<dir>` by their top-level string
/// `.status` field. Files without `.status` are excluded.
///
/// `total` = sum of every value in `counts`.
#[derive(Debug, Clone, Serialize)]
pub struct ArtifactStatusGroup {
    /// Project-relative dir (e.g. `.ccteam/issues`).
    pub dir: String,
    /// Sum of `counts.values()`.
    pub total: u64,
    /// Distinct `.status` string → file count.
    pub counts: BTreeMap<String, u64>,
}

/// Enumerate immediate subdirs of `<project>/.ccteam/` and, for each
/// non-infrastructure subdir, count `*.json` files grouped by their
/// top-level string `.status` field. Empty groups (no `.status`-bearing
/// files) are omitted.
///
/// Discovery skips hidden dirs (`.X`), trigger marker dirs
/// (`*-requests`), archived dirs (`*.archived`), and known
/// infrastructure (`rules`, `inbox`, `outbox`, `spawn_requests`).
///
/// Used by the web dashboard's "Artifact Status" panel to surface
/// open/fixing/closed/needs-human counts (or any project-defined status
/// enum) without ccteam-core knowing the schema.
///
/// Ordering: ASCII-sorted by `dir`.
pub fn artifact_status(slug: &str, paths: &CcteamPaths) -> Result<Vec<ArtifactStatusGroup>> {
    let root = paths.project_dir(slug).join(".ccteam");
    let Ok(rd) = std::fs::read_dir(&root) else {
        return Ok(Vec::new());
    };

    let mut groups: Vec<ArtifactStatusGroup> = Vec::new();
    for entry in rd.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        if !ft.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(String::from) else {
            continue;
        };
        if is_infra_dir(&name) {
            continue;
        }
        let counts = count_statuses(&entry.path());
        if counts.is_empty() {
            continue;
        }
        let total = counts.values().sum();
        groups.push(ArtifactStatusGroup {
            dir: format!(".ccteam/{name}"),
            total,
            counts,
        });
    }
    groups.sort_by(|a, b| a.dir.cmp(&b.dir));
    Ok(groups)
}

fn is_infra_dir(name: &str) -> bool {
    name.starts_with('.')
        || name.ends_with(".archived")
        || name.ends_with("-requests")
        || matches!(name, "rules" | "inbox" | "outbox" | "spawn_requests")
}

fn count_statuses(dir: &Path) -> BTreeMap<String, u64> {
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return counts;
    };
    for entry in rd.flatten() {
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&content) else {
            continue;
        };
        let Some(status) = value.get("status").and_then(|s| s.as_str()) else {
            continue;
        };
        *counts.entry(status.to_string()).or_insert(0) += 1;
    }
    counts
}

/// V0.4.6 F90 — one hourly cost bucket for the cost-history sparkline.
///
/// `hour` is the UTC hour-start RFC3339 timestamp (e.g.
/// `"2026-05-15T14:00:00Z"`). `cost_usd` is the sum of every
/// `agent_done.cost_usd` event whose `ts` falls inside `[hour,
/// hour+1h)`.
#[derive(Debug, Clone, Serialize)]
pub struct CostHistoryBucket {
    /// UTC hour-start RFC3339 timestamp.
    pub hour: String,
    /// Sum of `agent_done.cost_usd` across the hour.
    pub cost_usd: f64,
}

/// V0.4.6 F90 — bucket the slug's progress.jsonl `agent_done` events
/// by UTC hour over the most recent `window`. Returns one bucket per
/// hour in the window (sparse hours filled with `cost_usd = 0.0` so
/// the SPA can render an evenly-spaced sparkline without gap detection).
///
/// `window_hours` clamped to `[1, 24*30]` (cap at 30 days) to keep the
/// payload bounded.
///
/// Returns `Ok(vec![])` when the project has no progress.jsonl.
pub fn cost_history_buckets(
    slug: &str,
    paths: &CcteamPaths,
    window_hours: u32,
) -> Result<Vec<CostHistoryBucket>> {
    let hours = window_hours.clamp(1, 24 * 30);
    let now = chrono::Utc::now();
    // Truncate `now` to the start of the current hour so bucket
    // boundaries align cleanly.
    let now_hour = now
        .with_minute(0)
        .and_then(|t| t.with_second(0))
        .and_then(|t| t.with_nanosecond(0))
        .unwrap_or(now);
    let cutoff = now_hour - chrono::Duration::hours(hours as i64 - 1);

    let path = paths.progress_jsonl(slug);
    let events = if path.exists() {
        progress::read_all_events(&path).unwrap_or_default()
    } else {
        Vec::new()
    };

    // Pre-seed `hours` buckets so sparse data still renders a steady
    // x-axis on the SPA sparkline.
    let mut by_hour: BTreeMap<chrono::DateTime<Utc>, f64> = BTreeMap::new();
    for i in 0..hours {
        let hour = cutoff + chrono::Duration::hours(i as i64);
        by_hour.insert(hour, 0.0);
    }

    for event in &events {
        if event.get("event").and_then(|s| s.as_str()) != Some("agent_done") {
            continue;
        }
        let Some(ts_str) = event.get("ts").and_then(|s| s.as_str()) else {
            continue;
        };
        let Ok(ts) = chrono::DateTime::parse_from_rfc3339(ts_str) else {
            continue;
        };
        let ts_utc = ts.with_timezone(&Utc);
        if ts_utc < cutoff {
            continue;
        }
        // Round down to hour start.
        let hour = ts_utc
            .with_minute(0)
            .and_then(|t| t.with_second(0))
            .and_then(|t| t.with_nanosecond(0))
            .unwrap_or(ts_utc);
        let cost = event
            .get("cost_usd")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        *by_hour.entry(hour).or_insert(0.0) += cost;
    }

    let buckets: Vec<CostHistoryBucket> = by_hour
        .into_iter()
        .map(|(hour, cost)| CostHistoryBucket {
            hour: hour.to_rfc3339(),
            cost_usd: cost,
        })
        .collect();
    Ok(buckets)
}

/// V0.4.6 F90 — per-active-session snapshot rendered inside each agent
/// card's expanded "running sessions" list. Mirrors what
/// `current_agent_sessions_with_liveness` returns, plus a live
/// `cost_usd` read from `~/.claude/jobs/<job_id>/state.json`
/// (best-effort: `0.0` when state.json missing / unparseable).
#[derive(Debug, Clone, Serialize)]
pub struct ActiveSessionInfo {
    /// Agent role (matches `WorkflowSpec::agents` key).
    pub role: String,
    /// Internal session id (`agent_spawn::session_id`).
    pub session_id: String,
    /// Claude Code background job id, when the spawn was F61+; `None`
    /// for legacy / Codex sessions.
    pub job_id: Option<String>,
    /// Cwd as reported by `state.json`; `None` when state.json is
    /// missing or pre-cwd schema.
    pub cwd: Option<String>,
    /// `agent_spawn::ts` (RFC3339 string).
    pub started_at: String,
    /// Live cumulative cost from state.json (`cost_usd` or
    /// `cost_usd_total`); `0.0` when state.json missing.
    pub cost_usd: f64,
    /// Model id extracted from `state.json::respawnFlags` (value
    /// following `--model`). `None` when flag absent / state.json
    /// unreadable.
    pub model: Option<String>,
    /// Estimated context-window remaining percentage in `[0.0, 100.0]`,
    /// derived from the most recent `message.usage` block in the
    /// session JSONL transcript (`linkScanPath`) divided by the model's
    /// context window size. `None` until the transcript has been
    /// written (i.e. the agent hasn't yet completed its first turn) or
    /// when the model id is unknown.
    pub context_remaining_pct: Option<f64>,
}

/// V0.4.6 F90 — enumerate every project's "open" agent_spawn (no
/// matching agent_done) and decorate with live `state.json` data.
/// Powers the agent card's expanded session list.
///
/// Read-only; no progress events written.
pub fn active_sessions(slug: &str, paths: &CcteamPaths) -> Result<Vec<ActiveSessionInfo>> {
    let progress_path = paths.progress_jsonl(slug);
    if !progress_path.exists() {
        return Ok(Vec::new());
    }
    let events = progress::read_all_events(&progress_path).unwrap_or_default();

    // We need both the open spawn list AND each spawn's ts. Walk
    // events twice cheaply (events are tail-only N=workflow life).
    let opens = progress::open_agent_spawns(&events);

    // Build a (sid -> ts) lookup from agent_spawn events.
    let mut sid_to_ts: BTreeMap<String, String> = BTreeMap::new();
    for event in &events {
        if event.get("event").and_then(|s| s.as_str()) != Some("agent_spawn") {
            continue;
        }
        let Some(sid) = event.get("session_id").and_then(|s| s.as_str()) else {
            continue;
        };
        let ts = event
            .get("ts")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        sid_to_ts.entry(sid.to_string()).or_insert(ts);
    }

    let mut out: Vec<ActiveSessionInfo> = Vec::new();
    for (sid, job_id, role) in opens {
        let started_at = sid_to_ts.get(&sid).cloned().unwrap_or_default();
        // Probe state.json for live cwd + cost. We re-use the same
        // parser claude_job uses (parse_cc_state_json reads the same
        // `cost_usd*` keys + `cwd`/`workdir`).
        let probe = match &job_id {
            Some(id) => probe_active_session_state(id),
            None => SessionProbe::default(),
        };
        out.push(ActiveSessionInfo {
            role,
            session_id: sid,
            job_id,
            cwd: probe.cwd,
            started_at,
            cost_usd: probe.cost_usd,
            model: probe.model,
            context_remaining_pct: probe.context_remaining_pct,
        });
    }
    // Sort: role, then started_at ascending so consecutive sessions of
    // one role render together.
    out.sort_by(|a, b| {
        a.role
            .cmp(&b.role)
            .then_with(|| a.started_at.cmp(&b.started_at))
    });
    Ok(out)
}

#[derive(Debug, Default)]
struct SessionProbe {
    cwd: Option<String>,
    cost_usd: f64,
    model: Option<String>,
    context_remaining_pct: Option<f64>,
}

fn probe_active_session_state(job_id: &str) -> SessionProbe {
    let path = ccteam_harness::state_json_path(job_id);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return SessionProbe::default();
    };
    let Ok(value) = serde_json::from_str::<Value>(&raw) else {
        return SessionProbe::default();
    };
    let cwd = value
        .get("cwd")
        .or_else(|| value.get("workdir"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let cost_usd = value
        .get("cost_usd")
        .or_else(|| value.get("cost_usd_total"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let model = model_from_respawn_flags(&value);
    let context_remaining_pct = model
        .as_deref()
        .and_then(|m| context_remaining_for(&value, m));

    SessionProbe {
        cwd,
        cost_usd,
        model,
        context_remaining_pct,
    }
}

/// Find the value immediately after `--model` in
/// `state.json::respawnFlags`. Returns `None` when the array is
/// missing or the flag isn't present.
fn model_from_respawn_flags(state: &Value) -> Option<String> {
    let flags = state.get("respawnFlags")?.as_array()?;
    let mut it = flags.iter();
    while let Some(item) = it.next() {
        if item.as_str() == Some("--model") {
            return it.next().and_then(|v| v.as_str()).map(String::from);
        }
    }
    None
}

/// Model id → context window size in tokens. Claude Code's `[1m]`
/// suffix marks the 1M-context tier; everything else gets the standard
/// 200K window. Unknown providers fall back to 200K as well — the
/// percentage will still be useful as a relative trend even if the
/// absolute cap is wrong.
fn context_window_size(model: &str) -> u64 {
    if model.contains("[1m]") {
        1_000_000
    } else {
        200_000
    }
}

/// Best-effort compute the context-remaining percentage by tailing
/// the session JSONL and reading the most recent `message.usage`
/// block. Returns `None` when the JSONL can't be located, the file
/// empty, or no `usage` block exists.
fn context_remaining_for(state: &Value, model: &str) -> Option<f64> {
    let path = session_jsonl_path(state)?;
    let usage = last_usage_in_jsonl(&path)?;
    let used = usage.input_tokens
        + usage.cache_creation_input_tokens
        + usage.cache_read_input_tokens
        + usage.output_tokens;
    let window = context_window_size(model);
    if window == 0 {
        return None;
    }
    let pct = 100.0 * (1.0 - (used as f64 / window as f64));
    Some(pct.clamp(0.0, 100.0))
}

/// Resolve the absolute path of a Claude Code session transcript.
///
/// Prefers `state.json::linkScanPath` when populated (authoritative,
/// updated by claude-code's link-scan worker). When that field is
/// absent or null — which happens for freshly spawned agents before
/// the link-scan worker first ticks — derives the path from
/// `~/.claude/projects/<cwd-with-/-as-->/<sessionId>.jsonl`, matching
/// the layout claude-code uses on disk.
fn session_jsonl_path(state: &Value) -> Option<std::path::PathBuf> {
    if let Some(p) = state.get("linkScanPath").and_then(|v| v.as_str()) {
        if !p.is_empty() {
            return Some(std::path::PathBuf::from(p));
        }
    }
    let cwd = state.get("cwd").and_then(|v| v.as_str())?;
    let session_id = state.get("sessionId").and_then(|v| v.as_str())?;
    let home = dirs::home_dir()?;
    let encoded = cwd.replace('/', "-");
    Some(
        home.join(".claude")
            .join("projects")
            .join(encoded)
            .join(format!("{session_id}.jsonl")),
    )
}

#[derive(Debug, Default)]
struct UsageTokens {
    input_tokens: u64,
    cache_creation_input_tokens: u64,
    cache_read_input_tokens: u64,
    output_tokens: u64,
}

/// Find the most recent `.message.usage` object in a Claude Code
/// session JSONL. Reads the tail (last 64 KiB) so we don't pay for
/// long-running sessions whose transcripts grow to tens of MB.
fn last_usage_in_jsonl(path: &std::path::Path) -> Option<UsageTokens> {
    use std::io::{Read, Seek, SeekFrom};
    const TAIL_BYTES: u64 = 64 * 1024;

    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = String::new();
    file.read_to_string(&mut buf).ok()?;

    // Walk lines newest → oldest, parse each as JSON, and stop at the
    // first one carrying `.message.usage`. The tail slice may start
    // mid-line; we skip the first (possibly truncated) line on the
    // backwards walk to avoid mis-parsing a partial JSON object.
    let lines: Vec<&str> = buf.lines().collect();
    let skip_first = start > 0;
    for (idx, line) in lines.iter().enumerate().rev() {
        if skip_first && idx == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        let Some(usage) = value
            .get("message")
            .and_then(|m| m.get("usage"))
            .filter(|u| u.is_object())
        else {
            continue;
        };
        return Some(UsageTokens {
            input_tokens: usage
                .get("input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            cache_creation_input_tokens: usage
                .get("cache_creation_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            cache_read_input_tokens: usage
                .get("cache_read_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            output_tokens: usage
                .get("output_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        });
    }
    None
}

/// V0.4.6 F90 — read the tail of a claude bg job's `output.log`.
/// `tail_lines` clamped to `[1, 5000]` to bound the JSON payload.
///
/// Returns `Ok((tail_string, total_lines))`. When `output.log` is
/// missing returns `Ok(("", 0))` rather than 404 — the SPA's
/// `FailureInspector` then surfaces a "no log available" hint.
pub fn job_log_tail(job_id: &str, tail_lines: u32) -> Result<(String, u64)> {
    let n = tail_lines.clamp(1, 5000) as usize;
    let state_path = ccteam_harness::state_json_path(job_id);
    // output.log lives in the same job dir as state.json. state_json_path
    // returns `<base>/<job_id>/state.json`; replace the filename.
    let Some(job_dir) = state_path.parent() else {
        return Ok((String::new(), 0));
    };
    let log_path = job_dir.join("output.log");
    if !log_path.exists() {
        return Ok((String::new(), 0));
    }
    let body = std::fs::read_to_string(&log_path)
        .with_context(|| format!("read {}", log_path.display()))?;
    let lines: Vec<&str> = body.lines().collect();
    let total = lines.len() as u64;
    if lines.len() <= n {
        return Ok((body, total));
    }
    let tail = lines[lines.len() - n..].join("\n");
    Ok((tail, total))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use tempfile::TempDir;

    fn fake_paths(root: &std::path::Path) -> CcteamPaths {
        CcteamPaths {
            root: root.join(".ccteam"),
            projects_root: root.join("projects"),
        }
    }

    #[test]
    fn collect_projects_empty_when_root_missing() {
        let tmp = TempDir::new().unwrap();
        let paths = fake_paths(tmp.path());
        let out = collect_projects(&paths).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn collect_projects_skips_dirs_without_state_json() {
        let tmp = TempDir::new().unwrap();
        let paths = fake_paths(tmp.path());
        fs::create_dir_all(paths.projects_root.join("orphan")).unwrap();
        let out = collect_projects(&paths).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn collect_projects_loads_one_project() {
        let tmp = TempDir::new().unwrap();
        let paths = fake_paths(tmp.path());
        let slug = "dev-foo";
        let state_path = paths.project_state(slug);
        let state = ProjectState::initial(slug.to_string());
        state.save(&state_path).unwrap();

        let out = collect_projects(&paths).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].state.slug, slug);
    }

    /// V0.4.2 F73: a project registered in config.yaml but living
    /// outside `projects_root` (e.g. ~/code/<repo>) is still picked
    /// up by collect_projects.
    #[test]
    fn collect_projects_reads_registered_project_outside_projects_root() {
        let tmp = TempDir::new().unwrap();
        let paths = fake_paths(tmp.path());
        // Project lives at tmp/external/myapp, NOT under projects_root.
        let external = tmp.path().join("external").join("myapp");
        std::fs::create_dir_all(external.join(".ccteam")).unwrap();
        let state = ProjectState::initial("myapp".into());
        state
            .save(&external.join(".ccteam").join("state.json"))
            .unwrap();

        crate::config::append_project(
            &paths.root,
            crate::config::ProjectEntry {
                slug: "myapp".into(),
                path: external.clone(),
                team: "dev".into(),
                installed_at: Utc::now(),
            },
        )
        .unwrap();

        let out = collect_projects(&paths).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].state.slug, "myapp");
    }

    /// V0.4.2 F73 fallback path: legacy projects under `projects_root`
    /// without a config.yaml entry are still discovered. A project
    /// that exists in BOTH the registry and the walk path is reported
    /// once (registry wins, no duplicate).
    #[test]
    fn collect_projects_dedups_registered_and_walked_slugs() {
        let tmp = TempDir::new().unwrap();
        let paths = fake_paths(tmp.path());
        let slug = "shared";
        // Live project under projects_root (legacy fs walk path).
        let state_path = paths.project_state(slug);
        let state = ProjectState::initial(slug.to_string());
        state.save(&state_path).unwrap();
        // Register the SAME slug in config.yaml pointing at the same dir.
        crate::config::append_project(
            &paths.root,
            crate::config::ProjectEntry {
                slug: slug.into(),
                path: paths.project_dir(slug),
                team: "dev".into(),
                installed_at: Utc::now(),
            },
        )
        .unwrap();

        let out = collect_projects(&paths).unwrap();
        assert_eq!(out.len(), 1, "registry hit must not double-count");
        assert_eq!(out[0].state.slug, slug);
    }

    /// A registered project whose state.json went missing emits a
    /// warn log but does NOT abort the walk.
    #[test]
    fn collect_projects_skips_registered_with_missing_state_json() {
        let tmp = TempDir::new().unwrap();
        let paths = fake_paths(tmp.path());
        crate::config::append_project(
            &paths.root,
            crate::config::ProjectEntry {
                slug: "ghost".into(),
                path: tmp.path().join("nowhere"),
                team: "dev".into(),
                installed_at: Utc::now(),
            },
        )
        .unwrap();
        let out = collect_projects(&paths).unwrap();
        assert!(out.is_empty(), "missing state.json is skipped, not fatal");
    }

    #[test]
    fn collect_recent_events_missing_file_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let paths = fake_paths(tmp.path());
        let out = collect_recent_events(&paths, "nope", 50).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn collect_recent_events_tails_n_lines() {
        let tmp = TempDir::new().unwrap();
        let paths = fake_paths(tmp.path());
        let slug = "dev-foo";
        let path = paths.progress_jsonl(slug);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut body = String::new();
        for i in 0..10 {
            body.push_str(&format!("{}\n", json!({"event": "x", "i": i})));
        }
        fs::write(&path, body).unwrap();
        let out = collect_recent_events(&paths, slug, 3).unwrap();
        assert_eq!(out.len(), 3);
        // Tail = last 3 lines.
        assert_eq!(out[0]["i"], 7);
        assert_eq!(out[2]["i"], 9);
    }

    #[test]
    fn collect_recent_events_drops_corrupt_lines() {
        let tmp = TempDir::new().unwrap();
        let paths = fake_paths(tmp.path());
        let slug = "dev-foo";
        let path = paths.progress_jsonl(slug);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let body = format!(
            "{}\nnot-json-at-all\n{}\n",
            json!({"event": "ok", "i": 1}),
            json!({"event": "ok", "i": 2})
        );
        fs::write(&path, body).unwrap();
        let out = collect_recent_events(&paths, slug, 50).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["i"], 1);
        assert_eq!(out[1]["i"], 2);
    }

    #[test]
    fn artifact_status_groups_by_top_level_status_field() {
        let tmp = TempDir::new().unwrap();
        let paths = fake_paths(tmp.path());
        let slug = "dev-foo";
        let ccteam = paths.project_dir(slug).join(".ccteam");

        // issues/: 2 open, 1 closed, plus one file without `.status`
        // (should be excluded) and one non-JSON file (ignored).
        let issues = ccteam.join("issues");
        fs::create_dir_all(&issues).unwrap();
        fs::write(issues.join("1.json"), json!({"status": "open"}).to_string()).unwrap();
        fs::write(issues.join("2.json"), json!({"status": "open"}).to_string()).unwrap();
        fs::write(
            issues.join("3.json"),
            json!({"status": "closed"}).to_string(),
        )
        .unwrap();
        fs::write(
            issues.join("4.json"),
            json!({"title": "no status"}).to_string(),
        )
        .unwrap();
        fs::write(issues.join("README.md"), "not json").unwrap();

        // prs/: 1 merged.
        let prs = ccteam.join("prs");
        fs::create_dir_all(&prs).unwrap();
        fs::write(
            prs.join("100.json"),
            json!({"status": "merged"}).to_string(),
        )
        .unwrap();

        // Infrastructure dirs must be ignored even with status-bearing
        // JSON inside (would otherwise produce noise rows).
        for skip in [
            "explore-requests",
            "fix-requests",
            "issues.archived",
            "rules",
            "inbox",
            "outbox",
            "spawn_requests",
        ] {
            let d = ccteam.join(skip);
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("x.json"), json!({"status": "noise"}).to_string()).unwrap();
        }
        // Hidden dir likewise ignored.
        fs::create_dir_all(ccteam.join(".cache")).unwrap();
        fs::write(
            ccteam.join(".cache").join("x.json"),
            json!({"status": "noise"}).to_string(),
        )
        .unwrap();

        let out = artifact_status(slug, &paths).unwrap();
        assert_eq!(out.len(), 2, "got groups: {out:?}");
        assert_eq!(out[0].dir, ".ccteam/issues");
        assert_eq!(out[0].total, 3);
        assert_eq!(out[0].counts.get("open"), Some(&2));
        assert_eq!(out[0].counts.get("closed"), Some(&1));
        assert_eq!(out[1].dir, ".ccteam/prs");
        assert_eq!(out[1].total, 1);
        assert_eq!(out[1].counts.get("merged"), Some(&1));
    }

    #[test]
    fn model_from_respawn_flags_extracts_value() {
        let v = json!({
            "respawnFlags": [
                "--agent", "explorer",
                "--dangerously-skip-permissions",
                "--model", "deepseek-v4-pro[1m]",
            ]
        });
        assert_eq!(
            model_from_respawn_flags(&v),
            Some("deepseek-v4-pro[1m]".into())
        );
    }

    #[test]
    fn model_from_respawn_flags_none_when_flag_missing() {
        let v = json!({"respawnFlags": ["--agent", "explorer"]});
        assert_eq!(model_from_respawn_flags(&v), None);
    }

    #[test]
    fn context_window_size_matches_1m_suffix() {
        assert_eq!(context_window_size("claude-opus-4-7[1m]"), 1_000_000);
        assert_eq!(context_window_size("deepseek-v4-pro[1m]"), 1_000_000);
        assert_eq!(context_window_size("claude-sonnet-4-6"), 200_000);
        assert_eq!(context_window_size("unknown"), 200_000);
    }

    #[test]
    fn last_usage_in_jsonl_finds_most_recent() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("session.jsonl");
        let body = format!(
            "{}\n{}\n{}\n",
            json!({"type": "user", "message": {"role": "user", "content": "hi"}}),
            json!({
                "type": "assistant",
                "message": {
                    "role": "assistant",
                    "usage": {
                        "input_tokens": 100,
                        "cache_creation_input_tokens": 0,
                        "cache_read_input_tokens": 1000,
                        "output_tokens": 50
                    }
                }
            }),
            json!({
                "type": "assistant",
                "message": {
                    "role": "assistant",
                    "usage": {
                        "input_tokens": 200,
                        "cache_creation_input_tokens": 0,
                        "cache_read_input_tokens": 5000,
                        "output_tokens": 100
                    }
                }
            }),
        );
        fs::write(&path, body).unwrap();
        let usage = last_usage_in_jsonl(&path).unwrap();
        // Newest wins.
        assert_eq!(usage.input_tokens, 200);
        assert_eq!(usage.cache_read_input_tokens, 5000);
        assert_eq!(usage.output_tokens, 100);
    }

    #[test]
    fn context_remaining_for_computes_percentage() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("session.jsonl");
        // 100K context used out of 1M (deepseek [1m]) → 90% remaining.
        let usage = json!({
            "type": "assistant",
            "message": {
                "usage": {
                    "input_tokens": 0,
                    "cache_creation_input_tokens": 0,
                    "cache_read_input_tokens": 100_000,
                    "output_tokens": 0
                }
            }
        });
        fs::write(&path, format!("{usage}\n")).unwrap();
        let state = json!({"linkScanPath": path.to_str().unwrap()});
        let pct = context_remaining_for(&state, "deepseek-v4-pro[1m]").unwrap();
        assert!((pct - 90.0).abs() < 0.01, "got {pct}");
    }

    #[test]
    fn session_jsonl_path_prefers_link_scan_path_when_present() {
        let v = json!({
            "linkScanPath": "/abs/path/to/session.jsonl",
            "cwd": "/anywhere",
            "sessionId": "uuid",
        });
        let p = session_jsonl_path(&v).unwrap();
        assert_eq!(p.to_str().unwrap(), "/abs/path/to/session.jsonl");
    }

    #[test]
    fn session_jsonl_path_derives_from_cwd_and_session_id_when_link_scan_null() {
        let v = json!({
            "linkScanPath": null,
            "cwd": "/vol4/1000/nasworkspace/dex-ui",
            "sessionId": "c77f601e-1f7d-4449-a2b4-222f5a63ba1f",
        });
        let p = session_jsonl_path(&v).unwrap();
        // Derived path ends in the encoded cwd + session id.
        let suffix = "/.claude/projects/-vol4-1000-nasworkspace-dex-ui/\
                      c77f601e-1f7d-4449-a2b4-222f5a63ba1f.jsonl";
        assert!(p.to_str().unwrap().ends_with(suffix), "got {}", p.display());
    }

    #[test]
    fn context_remaining_for_none_when_jsonl_missing() {
        let state = json!({"linkScanPath": "/no/such/path.jsonl"});
        assert_eq!(context_remaining_for(&state, "claude-sonnet-4-6"), None);
    }

    #[test]
    fn artifact_status_returns_empty_when_no_ccteam_dir() {
        let tmp = TempDir::new().unwrap();
        let paths = fake_paths(tmp.path());
        let out = artifact_status("nope", &paths).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn collect_recent_events_merges_flex_session_streams() {
        let tmp = TempDir::new().unwrap();
        let paths = fake_paths(tmp.path());
        let slug = "flex-foo";
        let mut state = ProjectState::initial_for_team(slug.into(), "flex".into());
        state.team_kind = TeamKind::Flex;
        state.save(&paths.project_state(slug)).unwrap();

        let p1 = paths.progress_jsonl_for_session(slug, "claude-1");
        let p2 = paths.progress_jsonl_for_session(slug, "claude-2");
        fs::create_dir_all(p1.parent().unwrap()).unwrap();
        fs::write(
            &p1,
            format!(
                "{}\n{}\n",
                json!({"event": "a", "sid": "claude-1", "ts": "2026-05-10T00:00:01Z"}),
                json!({"event": "c", "sid": "claude-1", "ts": "2026-05-10T00:00:03Z"})
            ),
        )
        .unwrap();
        fs::write(
            &p2,
            format!(
                "{}\n",
                json!({"event": "b", "sid": "claude-2", "ts": "2026-05-10T00:00:02Z"})
            ),
        )
        .unwrap();

        let out = collect_recent_events(&paths, slug, 2).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["event"], "b");
        assert_eq!(out[1]["event"], "c");
    }
}
