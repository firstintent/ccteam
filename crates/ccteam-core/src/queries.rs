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
//! Architecture refs: `docs/v0-3/prd.md` §4 (M5.1 dashboard data
//! source), `docs/dev-coupling-audit.md` F45 (extends the M5.0
//! write-helper promotion to the read side), `docs/tech-design.md`
//! §5.5 progress.jsonl SoT.

use std::collections::HashMap;

use anyhow::{Context, Result};
use chrono::Utc;
use serde::Serialize;
use serde_json::Value;

use crate::paths::CcteamPaths;
use crate::progress::{
    self, current_agent_sessions, escalation_count, workflow_cost_total, AgentSessionStatus,
    AgentSessionSummary,
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
    /// Sum of cost across every `agent_done` event in the slice.
    pub total_cost_usd: f64,
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
    let escalation_count = escalation_count(&events);
    let sessions = current_agent_sessions(&events);

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
                // session.role not in spec — surface as a synthetic
                // role row so the UI can see it (orphan agent).
                let entry = by_role.entry(session.role.as_str()).or_insert(AgentStatus {
                    role: session.role.clone(),
                    running_count: 0,
                    queued_count: 0,
                    total_cost_usd: 0.0,
                    last_session_status: None,
                });
                accumulate_session(entry, session);
                last_by_role.insert(session.role.as_str(), session);
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

fn count_files_in_dir(dir: &std::path::Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    rd.flatten()
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .count() as u64
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
        state.save(&external.join(".ccteam").join("state.json"))
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
