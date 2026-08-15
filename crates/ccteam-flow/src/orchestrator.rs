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
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::task::JoinSet;

use crate::artifact_watcher::{ArtifactEvent, ArtifactWatcher};
use crate::workflow_watcher::{WorkflowFileEvent, WorkflowFileWatcher};
use ccteam_core::inbox::{InboxMessage, SessionMailbox};
use ccteam_core::paths::CcteamPaths;
use ccteam_core::spawn_brief::{render_spawn_brief, SpawnContext as SpawnBriefContext};
use ccteam_core::{daemon, handoff, preferences, progress, queries};
use ccteam_harness::execution::{ClaudeTuiAdapter, CodexExecAdapter};
use ccteam_harness::ClaudeBgAdapter;
use ccteam_harness::{
    AgentSpecBrief, AgentVendor, HarnessAdapter, SessionHandle, SpawnCtx, ThreadEvent,
    UnifiedTokenUsage,
};

use crate::workflow::{
    AgentSpec, AgentTeamSpec, Executor, Trigger, WorkflowError, WorkflowMode, WorkflowSpec,
};

/// V0.6.0 F107 — placeholder cost-from-usage estimator. Cost-crater
/// teammate will replace this with `ccteam_cost::estimate_cost(&usage,
/// vendor, model) -> f64`. Wave 1 returns 0 because the active
/// agent_done driver is still the F80 `claude_job::probe_job` poller
/// which reads `cost_usd` from `state.json` directly.
/// V0.6.0 Wave 3 F112 — derive vendor string from a
/// [`SessionHandle::harness`] tag. Used to annotate `agent_done`
/// events with the per-vendor key consumed by
/// [`ccteam_core::queries::CostSummary::cost_24h_by_vendor`] + per-vendor
/// budget caps. Anything starting with `"codex"` is classified codex;
/// everything else is claude.
pub fn vendor_from_harness(harness: &str) -> &'static str {
    if harness.starts_with("codex") {
        "codex"
    } else {
        "claude"
    }
}

#[allow(dead_code)] // Retained for transitional `translate_thread_event` callers; Wave 3 prefers ccteam_cost::estimate_cost.
fn usage_to_cost_placeholder(_usage: &UnifiedTokenUsage) -> f64 {
    0.0
}

/// Production model id; `[1m]` opts in to Claude Code's 1M context.
pub const DEFAULT_CLAUDE_MODEL: &str = "claude-sonnet-4-6[1m]";

/// Default budget ceiling (USD). Mirrors CLAUDE.md §三 "项目累计 cost
/// > $200 物理上限". F66 only blocks new spawns at this line — running
/// > sessions are never killed.
pub const DEFAULT_BUDGET_LIMIT_USD: f64 = 200.0;

/// Consecutive `start_thread` failures (per role) before meta-agent
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

/// V0.4.6 F82 — reason a project event_loop was asked to terminate
/// gracefully. The variant is recorded as `reason: "<value>"` on the
/// `workflow_done` event so postmortem reads can distinguish "user
/// disabled the workflow" from "daemon graceful shutdown" from "budget
/// cap auto-disable".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelReason {
    /// `workflow.yaml::enabled: false` flipped while loop was running.
    Disabled,
    /// `ccteam remove <slug>` (F81 wave 2) tore the project out.
    Removed,
    /// Spec changed in a way the watcher can't apply in-place (e.g.
    /// agents topology mutated); old loop ends + new one starts.
    Reloaded,
    /// Daemon-wide graceful shutdown (F86 wave 2 entry point).
    Shutdown,
    /// Budget cap tripped (F84 wave 2 entry point).
    BudgetExceeded,
}

impl CancelReason {
    /// Wire-format string written to `progress.jsonl::workflow_done.reason`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Removed => "removed",
            Self::Reloaded => "reloaded",
            Self::Shutdown => "shutdown",
            Self::BudgetExceeded => "budget_exceeded",
        }
    }
}

/// V0.5.0 F97 — `handle_agent_team_reload` outcome variants. Caller
/// (the daemon's main loop) dispatches on these to decide whether to
/// keep the running event loop (`HotApplied`), cancel without
/// re-spawning (`ColdRequired`), or fall through to the V0.4.6
/// blanket re-roster (`NotApplicable`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentTeamReloadOutcome {
    /// Hot diff written to lead inbox. Caller MUST NOT cancel the
    /// running event_loop; the lead picks up the change on its next
    /// idle tick via its `.ccteam/inbox/` polling.
    HotApplied,
    /// Cold diff (topology change). `workflow_done
    /// reason="cold_reload_required"` already emitted. Caller MUST
    /// cancel the running event_loop and MUST NOT re-spawn; user
    /// must run `ccteam start --restart-team <slug>`.
    ColdRequired,
    /// Not an agent-team-mode reload (artifact-driven, missing
    /// snapshot, or parse failure). Caller falls back to the V0.4.6
    /// blanket re-roster path.
    NotApplicable,
}

/// V0.5.0 F97 — best-effort reconstruction of `AgentTeamSpec` from
/// `.ccteam/team-snapshot.json`. The snapshot is a JSON blob written
/// by `spawn_agent_team_lead` containing the frozen
/// `cleanup_on_stop` + `suggested_teammates` + `team_name` +
/// `lead_session_id` + `teammate_mode`. We only need enough fields to
/// drive `AgentTeamSpec::classify_reload`; missing optional fields
/// default to sensible values.
fn snapshot_to_team_spec(snapshot: &serde_json::Value) -> Option<AgentTeamSpec> {
    use crate::workflow::{AgentTeamSpec, CleanupOnStop, SuggestedTeammate, SuggestedTeammateKind};
    let team_name = snapshot.get("team_name")?.as_str()?.to_string();
    let teammate_mode = snapshot
        .get("teammate_mode")
        .and_then(|v| v.as_str())
        .map(String::from);
    let cleanup_on_stop = match snapshot
        .get("cleanup_on_stop")
        .and_then(|v| v.as_str())
        .unwrap_or("force-kill")
    {
        "ask-lead" => CleanupOnStop::AskLead,
        "leave-running" => CleanupOnStop::LeaveRunning,
        _ => CleanupOnStop::ForceKill,
    };
    let auto_spawn_teammates = snapshot
        .get("auto_spawn_teammates")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let suggested_teammates = snapshot
        .get("suggested_teammates")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| {
                    let role = t.get("role")?.as_str()?.to_string();
                    let kind = match t.get("kind")?.as_str()? {
                        "ad-hoc" => SuggestedTeammateKind::AdHoc,
                        _ => SuggestedTeammateKind::Definition,
                    };
                    let spawn_brief = t
                        .get("spawn_brief")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    Some(SuggestedTeammate {
                        role,
                        kind,
                        spawn_brief,
                        adhoc_model: t
                            .get("adhoc_model")
                            .and_then(|v| v.as_str())
                            .map(String::from),
                        adhoc_color: t
                            .get("adhoc_color")
                            .and_then(|v| v.as_str())
                            .map(String::from),
                        adhoc_tools: t.get("adhoc_tools").and_then(|v| {
                            v.as_array().map(|arr| {
                                arr.iter()
                                    .filter_map(|x| x.as_str().map(String::from))
                                    .collect()
                            })
                        }),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Some(AgentTeamSpec {
        team_name,
        // lead_seed isn't snapshotted — `classify_reload` doesn't look
        // at it (it's a hot field), so empty is fine for the diff.
        lead_seed: String::new(),
        teammate_mode,
        cleanup_on_stop,
        snapshot_path: None,
        suggested_teammates,
        auto_spawn_teammates,
    })
}

/// V0.5.0 F95 + F94 — Anthropic Agent Teams event mirror. Six
/// variants. The first five (F95) are emitted by
/// [`crate::AgentTeamsWatcher`] into `~/.ccteam/teams-progress.jsonl`
/// from filesystem watching of `~/.claude/teams/<team>/`. The sixth
/// (`TeamTeammateIdle`, F94 Wave 2) is hook-only — Anthropic's idle
/// state is an in-memory signal not surfaced through the
/// `config.json` / `inboxes/` / `tasks/` files the watcher reads. The
/// legacy agent-team hook template that emitted this event was retired with
/// agent-team init; the enum remains only so deferred flow code can still read
/// old progress rows.
///
/// **Why a typed enum on top of `serde_json::Value`**: the watcher
/// writes events as untyped JSON (the legacy approach used by every
/// other event in this file), but F96 web SPA + cross-crate test
/// suites benefit from a compile-time-checked schema. The enum
/// `#[serde(tag = "event")]` shape matches the on-wire format
/// byte-for-byte, so a consumer can `serde_json::from_value` directly
/// without reflowing the watcher's emit path.
///
/// PRD: `docs/versions/v0-5-0/prd.md` §F95 + §F94 `event` table.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "event")]
pub enum TeamEvent {
    #[serde(rename = "team_member_joined")]
    TeamMemberJoined {
        team_name: String,
        teammate_name: String,
        agent_id: String,
        agent_type: String,
        model: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        color: Option<String>,
        cwd: String,
        backend_type: String,
        definition_backed: bool,
        /// RFC3339 derived from `members[i].joinedAt`.
        started_at: String,
        /// RFC3339 timestamp the watcher emitted this event.
        ts: String,
    },
    #[serde(rename = "team_member_left")]
    TeamMemberLeft {
        team_name: String,
        teammate_name: String,
        ts: String,
    },
    #[serde(rename = "team_message_sent")]
    TeamMessageSent {
        team_name: String,
        from: String,
        to: String,
        /// `text` truncated to
        /// `teams_inbox_parser::MAX_TEXT_LEN` chars (≤200).
        text_truncated: String,
        /// Anthropic-recorded message timestamp (ISO-8601).
        msg_ts: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        color: Option<String>,
        #[serde(default)]
        read: bool,
        ts: String,
    },
    #[serde(rename = "team_task_created")]
    TeamTaskCreated {
        team_name: String,
        task_id: String,
        title: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        assignee: Option<String>,
        #[serde(default)]
        dependencies: Vec<String>,
        ts: String,
    },
    #[serde(rename = "team_task_completed")]
    TeamTaskCompleted {
        team_name: String,
        task_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        result_summary: Option<String>,
        completed_at: String,
        ts: String,
    },
    /// V0.5.0 F94 — legacy `TeammateIdle` hook row. The hook template is
    /// retired, but deferred flow code keeps the variant for old progress rows.
    #[serde(rename = "team_teammate_idle")]
    TeamTeammateIdle {
        team_name: String,
        teammate_name: String,
        /// Hook payload's `idleReason` field (typically `"available"`
        /// or `"waiting"`). Optional because the Anthropic hook
        /// surface is still in flux.
        #[serde(skip_serializing_if = "Option::is_none")]
        idle_reason: Option<String>,
        /// RFC3339 timestamp from the hook payload (when the teammate
        /// transitioned into idle state).
        idle_since: String,
        /// RFC3339 timestamp the hook subprocess wrote the event.
        ts: String,
    },
}

/// V0.4.6 F84 — discover the workflow.yaml location for a project
/// (matches `WorkflowSpec::load_for_project`: prefer the root file,
/// then `.ccteam/`). Returns `None` if neither exists.
fn workflow_yaml_path_for(project_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let direct = project_dir.join("workflow.yaml");
    if direct.exists() {
        return Some(direct);
    }
    let nested = project_dir.join(".ccteam").join("workflow.yaml");
    if nested.exists() {
        return Some(nested);
    }
    None
}

/// V0.4.6 F84 — text-level patch of `enabled:` in workflow.yaml. The
/// goal is to preserve user comments + key ordering (which a serde
/// round-trip would lose). Logic:
///
/// 1. If a top-level `enabled:` line exists → rewrite its value to
///    `false`.
/// 2. Otherwise → insert `enabled: false` as a fresh top-level line
///    near the top of the file (after `name:` if present, else line 1).
///
/// Idempotent: a file already at `enabled: false` is left unchanged
/// (no write, no mtime bump, so an F82 watcher's debounce stays
/// quiet).
///
/// Returns an error only on IO failure; missing patterns are handled
/// by case (2) above so this never fails-loud on the YAML content
/// shape.
pub(crate) fn patch_workflow_yaml_enabled_false(path: &std::path::Path) -> std::io::Result<()> {
    let content = std::fs::read_to_string(path)?;
    let new = patch_enabled_false_in_yaml_str(&content);
    if new == content {
        return Ok(());
    }
    // Atomic-ish write: write to sibling .tmp then rename.
    let tmp = path.with_extension("yaml.tmp.f84");
    std::fs::write(&tmp, new)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Pure text transform — exposed for unit testing.
pub(crate) fn patch_enabled_false_in_yaml_str(content: &str) -> String {
    // Look for a top-level `enabled:` (zero-indent) line; only treat
    // it as the workflow-level enabled when it's at column 0 to avoid
    // touching nested `enabled:` inside agents.
    let mut found = false;
    let mut out_lines: Vec<String> = Vec::with_capacity(content.lines().count() + 1);
    for line in content.lines() {
        let trimmed = line.trim_start();
        let leading = line.len() - trimmed.len();
        if leading == 0 && trimmed.starts_with("enabled:") {
            found = true;
            // Preserve any trailing comment after the value.
            let after_colon = trimmed.trim_start_matches("enabled:").trim_start();
            // If there's a `#` comment, keep it.
            if let Some(hash) = after_colon.find('#') {
                let comment = &after_colon[hash..];
                out_lines.push(format!("enabled: false  {}", comment));
            } else {
                out_lines.push("enabled: false".to_string());
            }
        } else {
            out_lines.push(line.to_string());
        }
    }
    if !found {
        // Insert a fresh `enabled: false` line. Prefer "right after
        // `name:`" so it sits with the rest of the top-level scalars;
        // fall back to the very start.
        let insert_at = out_lines
            .iter()
            .position(|l| l.trim_start().starts_with("name:") && !l.starts_with(' '))
            .map(|i| i + 1)
            .unwrap_or(0);
        out_lines.insert(insert_at, "enabled: false".to_string());
    }
    let mut joined = out_lines.join("\n");
    // Preserve trailing newline if original had one.
    if content.ends_with('\n') && !joined.ends_with('\n') {
        joined.push('\n');
    }
    joined
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
    /// V0.4.6 F82 — per-slug cancellation senders. Filled by
    /// `spawn_new_rostered_projects` when a project event_loop starts.
    /// `unroster_project` / `reload_project` take the entry out + send
    /// the reason; `run_project_with_cancel` selects on the receiver
    /// and writes `workflow_done` before returning.
    /// F86 reuses this for `cancel_event_loop` + daemon-wide shutdown
    /// (CancelReason::Shutdown).
    cancel_handles: Arc<Mutex<HashMap<String, oneshot::Sender<CancelReason>>>>,
    /// V0.4.6 F82 — shared "already-spawned" slug set. Promoted from
    /// the local `HashSet` inside `run` so `reload_project` can pop a
    /// slug out + let the next rescan re-spawn it. `unroster_project`
    /// also removes the entry so F81 `ccteam remove` doesn't leak a
    /// ghost slug in the daemon's view.
    spawned: Arc<Mutex<HashSet<String>>>,
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

/// V0.8 W3 follow-up — completion probe for a mode-2 foreground-in-mux
/// bg spawn (`CCTEAM_CLAUDE_BG_VIA_MUX=1`). Such a spawn is a
/// `claude -p ... --agent <role>` foreground run owned by the mux
/// daemon: it runs the agentic loop to completion and exits, ending the
/// mux session. There is no `~/.claude/jobs/<id>/state.json` to poll, so
/// liveness == "does the mux session still exist".
///
/// - `exists == true`  → [`SessionStatus::Running`] (poll again next
///   tick; one extra "still running" tick while the daemon reaps is
///   harmless and matches the existing F80 cadence — no grace sleep).
/// - `exists == false` → [`SessionStatus::Done`] with `status =
///   "completed"`. The `claude -p` exit code (and thus completed-vs-
///   errored) is only available as a typed `ProcessExited` under
///   RmuxBackend (W2/W7); under the V0.8-default TmuxBackend the only
///   signal is session-end, so we report "completed" — same shape the
///   state.json path emits for a clean finish. Cost rollup for via_mux
///   spawns is deferred to the RmuxBackend bring-up wave (no per-job
///   state.json to read `cost_usd` from yet), so `cost_usd = None`.
/// - probe error → [`SessionStatus::Running`] (fail-safe: never retire a
///   spawn on a transient backend error; mirrors `session_status`'s
///   "unreadable state.json → still running" default).
async fn via_mux_session_status<B>(backend: &B, session: &str) -> SessionStatus
where
    B: ccteam_harness::ProcessBackend + ?Sized,
{
    match backend
        .exists(&ccteam_harness::MuxSessionId::new(session.to_string()))
        .await
    {
        Ok(true) => SessionStatus::Running,
        Ok(false) => SessionStatus::Done {
            cost_usd: None,
            status: "completed".to_string(),
        },
        Err(err) => {
            tracing::warn!(
                session,
                ?err,
                "via_mux liveness probe errored; treating spawn as still running this tick"
            );
            SessionStatus::Running
        }
    }
}

impl Orchestrator {
    /// Build orchestrator; pre-register claude + codex adapters so the
    /// dispatch path never allocates one mid-flight.
    pub fn new(paths: CcteamPaths, config: OrchestratorConfig) -> Result<Self> {
        let mut adapters: HashMap<&'static str, Arc<dyn HarnessAdapter + Send + Sync>> =
            HashMap::new();
        adapters.insert("claude", Arc::new(ClaudeBgAdapter::new()));
        adapters.insert("codex", Arc::new(CodexExecAdapter::new()));
        Ok(Self {
            paths,
            config,
            adapters,
            running: Arc::new(Mutex::new(HashMap::new())),
            pending: Arc::new(Mutex::new(HashMap::new())),
            fail_counts: Arc::new(Mutex::new(HashMap::new())),
            gate_states: Arc::new(Mutex::new(HashMap::new())),
            cost_accum: Arc::new(Mutex::new(0.0)),
            cancel_handles: Arc::new(Mutex::new(HashMap::new())),
            spawned: Arc::new(Mutex::new(HashSet::new())),
        })
    }

    pub fn paths(&self) -> &CcteamPaths {
        &self.paths
    }

    /// V0.4.6 F86 — F82 cancellation token alias. Sends
    /// `CancelReason::Shutdown` on the cancel channel registered for
    /// `slug` (if any). Returns `true` when a handle existed and the
    /// send was queued, `false` when the slug is not currently
    /// registered (loop already exited or never started).
    pub async fn cancel_event_loop(&self, slug: &str) -> bool {
        let sender = {
            let mut handles = self.cancel_handles.lock().await;
            handles.remove(slug)
        };
        match sender {
            Some(tx) => {
                let _ = tx.send(CancelReason::Shutdown);
                true
            }
            None => false,
        }
    }

    /// V0.4.6 F86 — orchestrator-wide graceful shutdown. Walks every
    /// registered cancel handle, fires it, and returns the slug list
    /// for the caller's timeout/abort logic. Idempotent (re-entry
    /// returns an empty vec).
    pub async fn shutdown(&self) -> Vec<String> {
        let handles: Vec<(String, oneshot::Sender<CancelReason>)> = {
            let mut map = self.cancel_handles.lock().await;
            map.drain().collect()
        };
        let mut slugs = Vec::with_capacity(handles.len());
        for (slug, tx) in handles {
            let _ = tx.send(CancelReason::Shutdown);
            slugs.push(slug);
        }
        slugs
    }

    /// Test-only: register a cancel handle without going through
    /// `run_project`. Used by `graceful_shutdown_test` to exercise the
    /// public cancel path in isolation.
    #[cfg(any(test, feature = "test-util"))]
    pub async fn test_register_cancel_handle(&self, slug: &str, tx: oneshot::Sender<CancelReason>) {
        self.cancel_handles
            .lock()
            .await
            .insert(slug.to_string(), tx);
    }

    /// Test-only mirror of the run-loop helper so tests can assert
    /// graceful-shutdown bookkeeping without bringing up the full
    /// daemon.
    #[cfg(any(test, feature = "test-util"))]
    pub async fn test_cancel_handles_len(&self) -> usize {
        self.cancel_handles.lock().await.len()
    }

    fn adapter_for(&self, exec: Executor) -> Option<&Arc<dyn HarnessAdapter + Send + Sync>> {
        let key: &'static str = match exec {
            Executor::Claude => "claude",
            Executor::Codex => "codex",
        };
        self.adapters.get(key)
    }

    /// V0.6.0 Wave 3 — dispatch helper that picks the right adapter for
    /// the `(executor, workflow_mode)` pair. mode 3 (`Chat`) overrides
    /// the per-executor bg/exec default with the long-running TUI adapter
    /// (Claude) or AppServer adapter (Codex; Wave 3 codex-exec teammate).
    ///
    /// Per the Wave 3 e2e-wiring contract: the orchestrator currently
    /// short-circuits `WorkflowMode::Chat` (lifecycle owned by
    /// `ccteam-im`); this helper is the seam future spawn paths reach
    /// through once orchestrator-owned chat scenarios land.
    pub fn pick_adapter(
        &self,
        exec: Executor,
        mode: WorkflowMode,
    ) -> Option<Arc<dyn HarnessAdapter + Send + Sync>> {
        match (exec, mode) {
            (Executor::Claude, WorkflowMode::Chat) => {
                Some(Arc::new(ClaudeTuiAdapter::new()) as Arc<dyn HarnessAdapter + Send + Sync>)
            }
            (Executor::Codex, WorkflowMode::Chat) => {
                // F173 — Codex chat bots route through `CodexExecAdapter`
                // (per-turn `codex exec --json` subprocess + advise
                // ledger hook in `submit_turn`). Always-fresh-handle
                // keeps the dispatch table from returning None for a
                // known `(vendor, mode)` pair, and matches the daemon's
                // `default_adapter_factory` so chat / bg / critic spawn
                // paths all funnel cost into the same ledger.
                Some(Arc::new(CodexExecAdapter::new()) as Arc<dyn HarnessAdapter + Send + Sync>)
            }
            (_, WorkflowMode::HumanApproval) => {
                // V0.6.1 F124 narrow scope — `mode: human-approval`
                // shares spawn mechanics with the per-executor bg
                // default (ClaudeBgAdapter / CodexExecAdapter). The
                // HITL gate is enforced at `poll_completions` time
                // (pending-drain skipped + `plan_decision_required`
                // emitted), not at adapter selection.
                //
                // TODO(V0.7-human-approval-adapter): introduce a
                //   dedicated `HumanApprovalAdapter` wrapper that
                //   delegates `spawn` / `submit_turn` to the inner
                //   adapter but tags spawn metadata for the IM
                //   round-trip + plan_decision injection (full F124
                //   scope, post-F98).
                // Reason deferred: V0.6.1 F124 + F98 already deliver
                //   the user-visible HITL contract via the
                //   poll-time gate, so the adapter wrapper is pure
                //   refactor (moves the gate from orchestrator into
                //   the adapter trait surface) with no behavioural
                //   delta. Pre-v1.0 it stays as the narrow-scope
                //   implementation rather than churn the
                //   `HarnessAdapter` trait for zero user impact.
                // Tracking: docs/versions/v0-6-6/prd.md §F168
                //   (decision row #6) + docs/versions/v0-6-1/prd.md
                //   §F124 + docs/dev-coupling-audit.md V0.6.6 segment.
                self.adapter_for(exec).cloned()
            }
            _ => self.adapter_for(exec).cloned(),
        }
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
        self.run_project_with_cancel(slug, None).await
    }

    /// V0.4.6 F82 — same as [`run_project`] but selects on `cancel_rx`
    /// for graceful termination. Receiving on the channel writes a
    /// `workflow_done { reason }` event before returning Ok(()). Passing
    /// `None` keeps the V0.4.5 behaviour (loop ends naturally when
    /// gates fire or the artifact channel closes). F86 reuses this for
    /// daemon-wide shutdown by sending `CancelReason::Shutdown`.
    ///
    /// **Returns Ok even on cancel** — cancellation is a normal exit,
    /// not an error. Real errors (workflow.yaml not found, artifact
    /// watcher init failure) still bubble.
    pub async fn run_project_with_cancel(
        &self,
        slug: &str,
        cancel_rx: Option<oneshot::Receiver<CancelReason>>,
    ) -> Result<()> {
        let project_dir = self.paths.project_dir(slug);
        let progress_path = self.paths.progress_jsonl(slug);

        let mut spec = WorkflowSpec::load_for_project(&project_dir).map_err(|e| match e {
            WorkflowError::NotFound(p) => anyhow::anyhow!("workflow.yaml not found in {:?}", p),
            other => anyhow::anyhow!(other),
        })?;

        // V0.4.6 F82 — `enabled: false` short-circuits roster: write
        // `workflow_done reason="disabled"` and return. The project's
        // workflow.yaml + state.json + progress.jsonl are otherwise
        // untouched, so flipping the field back to `true` (with the
        // workflow_watcher re-running this function) resumes cleanly.
        if !spec.enabled {
            tracing::info!(slug, "workflow disabled (enabled: false); skipping roster");
            progress::append_event(
                &progress_path,
                &json!({
                    "event": "workflow_done",
                    "workflow": spec.name,
                    "slug": slug,
                    "reason": "disabled",
                    "ts": Utc::now().to_rfc3339(),
                }),
            )?;
            return Ok(());
        }

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

        // V0.6.0 Wave 3 — `mode: chat` is owned by `ccteam-im` end to
        // end. The daemon spawns the tmux session via
        // `ClaudeTuiAdapter::start_thread`, drives turns from IM input,
        // and tails `turns.jsonl` for outbound. Orchestrator's role is
        // limited to logging `workflow_start` (above) so the project
        // appears in `ccteam status`; it must NOT enter the artifact
        // event loop nor attempt to spawn agents (the spec's `agents`
        // map is allowed to be empty in mode: chat per workflow.rs
        // validation).
        if matches!(spec.mode, WorkflowMode::Chat) {
            tracing::info!(
                slug,
                "mode: chat workflow — ccteam-im owns lifecycle; orchestrator exiting"
            );
            return Ok(());
        }

        // V0.4.5: pass the progress.jsonl file (so the watcher can
        // append `artifact_dir_created` events) — the previous version
        // passed `project_dir` (a directory), which made
        // `progress::append_event` fail with "open <project_dir>".
        //
        // V0.6.3 F145 — when a `squad:` block is declared, hand the
        // absolute squad routing dir to the watcher so the leader's
        // `<member>--*.md` artifacts surface as `ArtifactEvent`s tagged
        // with the squad route sentinel.
        let squad_root = spec
            .squad
            .as_ref()
            .map(|_| project_dir.join(crate::workflow::SQUAD_ROUTING_DIR));
        let (watcher, rx) =
            ArtifactWatcher::new(&spec, Some(progress_path.as_path()), squad_root.as_deref())?;
        let watcher_handle = watcher.start();

        self.dispatch_initial_triggers(slug, &spec).await?;

        let event_loop_fut = self.event_loop(slug, &spec, &project_dir, &progress_path, rx);

        let res = match cancel_rx {
            Some(rx) => {
                tokio::select! {
                    res = event_loop_fut => res,
                    recv_res = rx => {
                        // Sender dropped without sending (orchestrator
                        // didn't intend cancel) → treat as natural end.
                        // Sender sent a reason → write workflow_done + return.
                        match recv_res {
                            Ok(reason) => {
                                tracing::info!(
                                    slug,
                                    reason = reason.as_str(),
                                    "event_loop cancellation received",
                                );
                                let _ = progress::append_event(
                                    &progress_path,
                                    &json!({
                                        "event": "workflow_done",
                                        "workflow": spec.name,
                                        "slug": slug,
                                        "reason": reason.as_str(),
                                        "ts": Utc::now().to_rfc3339(),
                                    }),
                                );
                                Ok(())
                            }
                            Err(_canceled) => {
                                tracing::debug!(
                                    slug,
                                    "cancel sender dropped; event_loop ends naturally",
                                );
                                Ok(())
                            }
                        }
                    }
                }
            }
            None => event_loop_fut.await,
        };

        watcher_handle.abort();
        res
    }

    /// Daemon entry point: roster every project under
    /// `paths.projects_root` that has a `workflow.yaml`, drive each
    /// through [`run_project`] on its own tokio task, and keep the
    /// heartbeat file fresh so [`ccteam_core::daemon::check_health`] reports
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

        // V0.4.6 F85: one-shot `~/.claude/jobs/` GC at daemon startup.
        // Runs on the blocking pool so the `read_dir` + `remove_dir_all`
        // walk doesn't stall the event-loop. Reads the retention value
        // from `~/.ccteam/config.yaml::claude_jobs_retention_days`
        // (default 7 days; 0 disables GC). The sweep is best-effort:
        // any IO error is logged + swallowed so a transient
        // permissions glitch never blocks daemon boot.
        let gc_paths_root = self.paths.root.clone();
        tokio::task::spawn_blocking(move || {
            let retention = match ccteam_core::config::load(&gc_paths_root) {
                Ok(cfg) => cfg.claude_jobs_retention_days,
                Err(err) => {
                    tracing::warn!(
                        ?err,
                        "claude_jobs gc: failed to load config; using default retention"
                    );
                    ccteam_core::config::default_claude_jobs_retention_days()
                }
            };
            if retention == 0 {
                tracing::info!("claude_jobs gc: disabled (retention == 0)");
                return;
            }
            match ccteam_core::claude_job::gc_user_claude_jobs(retention, false) {
                Ok(report) => {
                    tracing::info!(
                        retention_days = retention,
                        dir_count_before = report.dir_count_before,
                        dir_count_after = report.dir_count_after,
                        removed = report.removed,
                        kept_working = report.kept_working,
                        kept_recent = report.kept_recent,
                        kept_corrupt = report.kept_corrupt,
                        kept_unknown = report.kept_unknown,
                        "claude_jobs gc completed"
                    );
                }
                Err(err) => {
                    tracing::warn!(?err, "claude_jobs gc failed; continuing daemon boot");
                }
            }
        });

        self.spawn_new_rostered_projects(&mut tasks, "startup")
            .await;

        // V0.4.6 F82 — install the workflow.yaml file watcher across
        // every rostered project so edits hot-reload without daemon
        // restart. We rebuild the watcher whenever the rescan tick
        // picks up new slugs (cheaper than incrementally registering)
        // — see `rebuild_workflow_watcher` for the swap mechanic.
        let (mut workflow_watcher_rx, _watcher_handle) = self.start_workflow_watcher().await;

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
                    // V0.4.6 F86 — graceful shutdown via cancel token.
                    // Replaces the V0.4.5 hard `abort_all()` path that
                    // dropped in-flight `workflow_done` writes and left
                    // phantom `agent_spawn` rows for F80 to mop up next
                    // start. The cancel path lets each `event_loop`
                    // emit `workflow_done reason="shutdown"` cleanly.
                    let slugs = self.shutdown().await;
                    tracing::info!(
                        count = slugs.len(),
                        "graceful shutdown begin: cancel signals dispatched"
                    );
                    let timed = tokio::time::timeout(
                        Duration::from_secs(30),
                        async {
                            while tasks.join_next().await.is_some() {}
                        },
                    )
                    .await;
                    if timed.is_err() {
                        tracing::warn!(
                            "graceful shutdown timeout (30s); falling back to abort_all() — \
                             in-flight progress.jsonl writes may be lost for stalled loops"
                        );
                        tasks.abort_all();
                        while tasks.join_next().await.is_some() {}
                    } else {
                        tracing::info!("graceful shutdown clean");
                    }
                    daemon::remove_heartbeat(&self.paths);
                    return Ok(());
                }
                _ = hb_ticker.tick() => {
                    if let Err(err) = daemon::write_heartbeat(&self.paths) {
                        tracing::warn!(?err, "heartbeat write failed");
                    }
                }
                _ = rescan_ticker.tick() => {
                    let newly_added = self
                        .spawn_new_rostered_projects(&mut tasks, "rescan")
                        .await;
                    if newly_added > 0 {
                        // Rebuild the file watcher so the new slugs
                        // pick up workflow.yaml edits immediately.
                        let (new_rx, _new_handle) = self.start_workflow_watcher().await;
                        workflow_watcher_rx = new_rx;
                    }
                }
                Some(evt) = workflow_watcher_rx.recv() => {
                    tracing::info!(
                        slug = evt.slug.as_str(),
                        ?evt.kind,
                        "workflow.yaml change detected; reloading project",
                    );
                    let slug = evt.slug.clone();
                    // V0.5.0 F97 — agent-team mode workflows have
                    // bespoke hot/cold-reload rules. Classify the diff
                    // before the blanket re-roster:
                    //   - hot (lead_seed / cosmetic) → write inbox msg,
                    //     do NOT cancel the running event loop.
                    //   - cold (team_name / topology) → emit
                    //     workflow_done reason="cold_reload_required",
                    //     clear watch, do NOT re-roster. User must
                    //     explicitly run `ccteam start --restart-team`.
                    //   - everything else (artifact-driven, or
                    //     agent-team without a frozen snapshot yet) →
                    //     fall through to the V0.4.6 blanket re-roster.
                    let agent_team_outcome = self
                        .handle_agent_team_reload(&slug)
                        .await;
                    match agent_team_outcome {
                        AgentTeamReloadOutcome::HotApplied => {
                            // No cancel / re-roster needed; the diff
                            // was applied via lead inbox.
                            continue;
                        }
                        AgentTeamReloadOutcome::ColdRequired => {
                            // workflow_done already emitted; cancel
                            // the running loop but do NOT re-spawn —
                            // user must run --restart-team explicitly.
                            self.unroster_project(
                                &slug,
                                CancelReason::Reloaded,
                            )
                            .await;
                            self.spawned.lock().await.remove(&slug);
                            // Rebuild watcher so the next workflow.yaml
                            // edit on this slug is still observed.
                            let (new_rx, _new_handle) = self.start_workflow_watcher().await;
                            workflow_watcher_rx = new_rx;
                            continue;
                        }
                        AgentTeamReloadOutcome::NotApplicable => {
                            // artifact-driven or no frozen snapshot —
                            // fall through to the V0.4.6 path below.
                        }
                    }
                    self.reload_project(&slug).await;
                    // Re-spawn immediately so reload latency stays
                    // well under the 5s acceptance bar (rather than
                    // waiting up to ROSTER_RESCAN_INTERVAL=10s).
                    let _ = self
                        .spawn_new_rostered_projects(&mut tasks, "reload")
                        .await;
                    // Rebuild watcher so the just-reloaded slug
                    // (which lost its registration via remove_project)
                    // is observed again on subsequent edits.
                    let (new_rx, _new_handle) = self.start_workflow_watcher().await;
                    workflow_watcher_rx = new_rx;
                }
                Some(joined) = tasks.join_next() => {
                    match joined {
                        Ok((slug, Ok(()))) => {
                            tracing::info!(slug, "project event loop ended cleanly");
                            // F82/F86: clear the orphaned cancel sender
                            // ONLY if no successor (`reload_project`) has
                            // already re-registered for this slug — else
                            // we'd silently drop the fresh tx and the new
                            // loop's cancel_rx would resolve Err on the
                            // next select! tick (V0.4.6 reload race bug
                            // — observed dex-ui losing its loop within
                            // 44µs of workflow.yaml mtime change).
                            if !self.spawned.lock().await.contains(&slug) {
                                self.cancel_handles.lock().await.remove(&slug);
                            }
                        }
                        Ok((slug, Err(err))) => {
                            tracing::warn!(slug, error = ?err, "project event loop errored");
                            if !self.spawned.lock().await.contains(&slug) {
                                self.cancel_handles.lock().await.remove(&slug);
                            }
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

    /// V0.4.6 F82 — build a [`WorkflowFileWatcher`] across every
    /// slug currently in `self.spawned`. Called on daemon startup +
    /// after each rescan that picks up a new slug.
    ///
    /// Returns `(receiver, JoinHandle)`. The handle is intentionally
    /// not awaited; dropping it is fine because the watcher task
    /// keeps running as long as the receiver is alive (it polls
    /// `tx.is_closed()` and exits when the orchestrator's receiver
    /// drops).
    async fn start_workflow_watcher(
        self: &Arc<Self>,
    ) -> (
        mpsc::Receiver<WorkflowFileEvent>,
        tokio::task::JoinHandle<()>,
    ) {
        let slugs: Vec<String> = self.spawned.lock().await.iter().cloned().collect();
        let projects: Vec<(String, PathBuf)> = slugs
            .into_iter()
            .map(|s| {
                let dir = self.paths.project_dir(&s);
                (s, dir)
            })
            .collect();
        match WorkflowFileWatcher::new(&projects) {
            Ok((watcher, rx)) => (rx, watcher.start()),
            Err(err) => {
                tracing::warn!(
                    ?err,
                    "workflow_watcher: failed to build; hot-reload disabled"
                );
                // Return a closed channel — the select arm becomes
                // dormant but the orchestrator keeps running.
                let (_tx, rx) = mpsc::channel::<WorkflowFileEvent>(1);
                let handle = tokio::spawn(async {});
                (rx, handle)
            }
        }
    }

    /// Walk the projects root and spawn event loops for slugs not yet
    /// in `self.spawned`. Shared between startup (`origin = "startup"`),
    /// the periodic rescan tick (`origin = "rescan"`), and the F82
    /// reload path (`origin = "reload"`). Slugs without a
    /// `workflow.yaml` are skipped silently — those are legacy V0.3.x
    /// phase-driven projects with no event-loop equivalent.
    ///
    /// Returns the number of newly-spawned slugs so the caller (the
    /// main `run` loop) can decide whether to rebuild the
    /// workflow file watcher.
    async fn spawn_new_rostered_projects(
        self: &Arc<Self>,
        tasks: &mut JoinSet<(String, Result<()>)>,
        origin: &'static str,
    ) -> usize {
        let projects = match queries::collect_projects(&self.paths) {
            Ok(p) => p,
            Err(err) => {
                tracing::warn!(?err, origin, "collect_projects failed; roster unchanged");
                return 0;
            }
        };
        let mut newly_added = 0;
        for proj in projects {
            let slug = proj.state.slug.clone();
            {
                let s = self.spawned.lock().await;
                if s.contains(&slug) {
                    continue;
                }
            }
            let project_dir = self.paths.project_dir(&slug);
            if !project_dir.join("workflow.yaml").exists()
                && !project_dir.join(".ccteam").join("workflow.yaml").exists()
            {
                tracing::debug!(slug, "no workflow.yaml; skipping (pre-V0.4.0 project)");
                continue;
            }
            match origin {
                "startup" => tracing::info!(slug, "starting project event loop"),
                "rescan" => tracing::info!(slug, "hot-loaded new project; starting event loop"),
                "reload" => tracing::info!(slug, "reload re-spawning project event loop"),
                _ => tracing::info!(slug, origin, "starting project event loop"),
            }

            // V0.4.6 F82/F86 — register per-slug cancellation token
            // BEFORE spawning so a racing `unroster_project` /
            // `reload_project` / `shutdown` can't miss the slug. The
            // sender goes into `self.cancel_handles` so all three paths
            // fire a graceful workflow_done.
            let (cancel_tx, cancel_rx) = oneshot::channel::<CancelReason>();
            self.cancel_handles
                .lock()
                .await
                .insert(slug.clone(), cancel_tx);

            let orch = Arc::clone(self);
            let slug_for_task = slug.clone();
            tasks.spawn(async move {
                let res = orch
                    .run_project_with_cancel(&slug_for_task, Some(cancel_rx))
                    .await;
                (slug_for_task, res)
            });
            self.spawned.lock().await.insert(slug);
            newly_added += 1;
        }
        newly_added
    }

    // ---- V0.4.6 F82: lifecycle control surface --------------------------

    /// V0.4.6 F82 — gracefully terminate a project event_loop. Used by
    /// F81 `ccteam remove <slug>`, F84 budget cap, F86 graceful daemon
    /// shutdown. Behaviour:
    ///
    /// 1. Look up + remove the cancel sender from `self.cancel_handles`.
    /// 2. Send `reason` through the channel — the running
    ///    `run_project_with_cancel` selects on it, appends a
    ///    `workflow_done { reason }` event to progress.jsonl, and
    ///    returns Ok(()).
    /// 3. The orchestrator's main `run` loop will see the task end via
    ///    `tasks.join_next` shortly after.
    /// 4. The slug stays in `self.spawned` so the rescan tick doesn't
    ///    re-add it — F81 is supposed to be terminal.
    ///
    /// **Returns true iff a cancel was sent.** Returns false when:
    /// - The slug had no event_loop running (not in cancel_handles).
    /// - The cancel sender was already taken (raced with another caller).
    /// - The task receiver was dropped before we sent (rare; usually
    ///   means the loop already ended naturally).
    pub async fn unroster_project(&self, slug: &str, reason: CancelReason) -> bool {
        let maybe_tx = self.cancel_handles.lock().await.remove(slug);
        let Some(tx) = maybe_tx else {
            tracing::debug!(slug, "unroster_project: no cancel handle (already ended)");
            return false;
        };
        if tx.send(reason).is_err() {
            tracing::debug!(
                slug,
                reason = reason.as_str(),
                "unroster_project: receiver dropped (loop already ended)",
            );
            return false;
        }
        tracing::info!(
            slug,
            reason = reason.as_str(),
            "unroster_project: cancel sent"
        );
        true
    }

    /// V0.4.6 F82 — terminate the current event_loop AND clear the
    /// `spawned` entry so the next rescan / manual `spawn_new_*` call
    /// re-spawns from the on-disk `workflow.yaml`. Used by the F82
    /// workflow.yaml file watcher (edit → reload) and by F84
    /// budget-exceeded → enabled=false flip.
    ///
    /// Mirror of [`unroster_project`] semantics except: the slug is
    /// also removed from `self.spawned` so it can be re-rostered.
    ///
    /// Always uses [`CancelReason::Reloaded`] — callers that need a
    /// specific reason (`Disabled`, `BudgetExceeded`) should call
    /// [`unroster_project`] explicitly with the right variant.
    pub async fn reload_project(&self, slug: &str) -> bool {
        let cancelled = self.unroster_project(slug, CancelReason::Reloaded).await;
        // Remove from `spawned` regardless of cancel success — a slug
        // that ended naturally still needs the `spawned` entry cleared
        // before a fresh task can take its place.
        self.spawned.lock().await.remove(slug);
        cancelled
    }

    /// V0.4.6 F82 — accessor for the per-slug cancel-handle map size,
    /// used by tests + future F86 shutdown to count live event_loops
    /// before firing the global cancel.
    pub async fn rostered_slug_count(&self) -> usize {
        self.cancel_handles.lock().await.len()
    }

    /// V0.4.6 F82 — true iff the slug has a live cancellation handle
    /// registered (i.e. its event_loop is running). Used by F86
    /// shutdown to enumerate slugs to cancel.
    pub async fn is_slug_rostered(&self, slug: &str) -> bool {
        self.cancel_handles.lock().await.contains_key(slug)
    }

    /// V0.5.0 F97 — agent-team mode hot-reload dispatch. Reads the
    /// project's workflow.yaml + `.ccteam/team-snapshot.json` (the
    /// frozen `AgentTeamSpec` from the last spawn) and classifies the
    /// diff:
    ///
    /// - **Hot**: writes a user-turn `.ccteam/inbox/<ts>-reload-*.md`
    ///   so the lead picks the new `lead_seed` / cosmetic tweak on its
    ///   next turn. Returns [`AgentTeamReloadOutcome::HotApplied`].
    /// - **Cold**: emits
    ///   `workflow_done reason="cold_reload_required"` to
    ///   `progress.jsonl` so the operator's web SPA + CLI surface
    ///   show the gate. Caller cancels the loop and DOES NOT
    ///   re-spawn — user must run
    ///   `ccteam start --restart-team <slug>` to bring up a fresh
    ///   lead matching the new topology. Returns
    ///   [`AgentTeamReloadOutcome::ColdRequired`].
    /// - **Not applicable**: artifact-driven workflow, OR agent-team
    ///   workflow without a `.ccteam/team-snapshot.json` (no spawn
    ///   yet — let the V0.4.6 blanket re-roster proceed).
    ///   Returns [`AgentTeamReloadOutcome::NotApplicable`].
    async fn handle_agent_team_reload(&self, slug: &str) -> AgentTeamReloadOutcome {
        let project_dir = self.paths.project_dir(slug);
        let spec = match WorkflowSpec::load_for_project(&project_dir) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(
                    slug,
                    ?err,
                    "F97 agent-team reload: parse failed; \
                                            falling back to blanket re-roster"
                );
                return AgentTeamReloadOutcome::NotApplicable;
            }
        };
        let WorkflowMode::AgentTeam = spec.mode else {
            return AgentTeamReloadOutcome::NotApplicable;
        };
        let Some(new_team) = spec.agent_team.as_ref() else {
            return AgentTeamReloadOutcome::NotApplicable;
        };

        let snapshot_path = project_dir.join(".ccteam").join("team-snapshot.json");
        let raw = match std::fs::read_to_string(&snapshot_path) {
            Ok(s) => s,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(
                    slug,
                    "F97 agent-team reload: no team-snapshot.json yet (lead not spawned); \
                     deferring to blanket re-roster",
                );
                return AgentTeamReloadOutcome::NotApplicable;
            }
            Err(err) => {
                tracing::warn!(slug, ?err, "F97 agent-team reload: snapshot read failed");
                return AgentTeamReloadOutcome::NotApplicable;
            }
        };
        let snapshot: serde_json::Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!(slug, ?err, "F97 agent-team reload: snapshot parse failed");
                return AgentTeamReloadOutcome::NotApplicable;
            }
        };
        // Reconstruct the OLD spec from the snapshot for diffing.
        let Some(old_team) = snapshot_to_team_spec(&snapshot) else {
            tracing::warn!(slug, "F97 agent-team reload: snapshot schema unrecognized");
            return AgentTeamReloadOutcome::NotApplicable;
        };

        let progress_path = self.paths.progress_jsonl(slug);
        match old_team.classify_reload(new_team) {
            Some(reason) => {
                tracing::info!(
                    slug,
                    reason = reason.as_str(),
                    "F97 agent-team reload: cold reload required",
                );
                let _ = progress::append_event(
                    &progress_path,
                    &json!({
                        "event": "workflow_done",
                        "workflow": spec.name,
                        "slug": slug,
                        "reason": "cold_reload_required",
                        "detail": reason,
                        "hint": format!(
                            "Run `ccteam start --restart-team {slug}` to spawn a fresh \
                             lead matching the new topology.",
                        ),
                        "ts": Utc::now().to_rfc3339(),
                    }),
                );
                AgentTeamReloadOutcome::ColdRequired
            }
            None => {
                // Hot reload: write the new lead_seed / cosmetic
                // tweak to the lead's inbox.
                let inbox_dir = project_dir.join(".ccteam").join("inbox");
                if let Err(err) = std::fs::create_dir_all(&inbox_dir) {
                    tracing::warn!(
                        slug,
                        ?err,
                        path = %inbox_dir.display(),
                        "F97 agent-team reload: failed to create inbox dir",
                    );
                    return AgentTeamReloadOutcome::NotApplicable;
                }
                let now = Utc::now();
                let filename = format!("{}-reload-update.md", now.format("%Y%m%dT%H%M%SZ"),);
                let msg_path = inbox_dir.join(&filename);
                let body = format!(
                    "---\nsource: ccteam-reload\npriority: normal\n---\n\n\
                     # workflow.yaml hot-reload\n\n\
                     The project's workflow.yaml `agent_team.lead_seed` (or a cosmetic \
                     teammate field) was updated. Treat the following as the new \
                     user-turn message and continue your existing plan accordingly. \
                     This is NOT a system prompt; honor it the same way you would a \
                     direct user request.\n\n\
                     ## Updated lead_seed\n\n{}\n",
                    new_team.lead_seed,
                );
                if let Err(err) = std::fs::write(&msg_path, body) {
                    tracing::warn!(
                        slug,
                        ?err,
                        path = %msg_path.display(),
                        "F97 agent-team reload: failed to write inbox message",
                    );
                    return AgentTeamReloadOutcome::NotApplicable;
                }
                tracing::info!(
                    slug,
                    path = %msg_path.display(),
                    "F97 agent-team reload: hot diff applied via lead inbox",
                );
                AgentTeamReloadOutcome::HotApplied
            }
        }
    }

    // ---- dispatch helpers ---------------------------------------------------

    async fn dispatch_initial_triggers(&self, slug: &str, spec: &WorkflowSpec) -> Result<()> {
        for (role, agent) in &spec.agents {
            match &agent.trigger {
                Trigger::Manual => {
                    tracing::info!(slug, role = role.as_str(), "waiting for explicit trigger");
                }
                Trigger::Schedule => {
                    // V0.6.3 F142 — the per-tick `check_schedules` pass
                    // anchors the cursor on first observation and fires
                    // on cron-due. Nothing to register here.
                    tracing::info!(
                        slug,
                        role = role.as_str(),
                        schedule = agent.schedule.as_deref().unwrap_or(""),
                        "schedule agent registered"
                    );
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

        // F82/F86: cancellation is handled by the outer `select!` in
        // `run_project_with_cancel`; here we just process artifact +
        // completion ticks and exit naturally when `rx` closes.
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
                    // V0.6.3 F142 — cron-scheduled agents. Runs after
                    // gates so a tick that just released a gate still
                    // re-checks schedules in the same pass.
                    self.check_schedules(slug, spec, project_dir, progress_path).await;
                    self.check_inbox(slug, spec, project_dir, progress_path).await;
                    self.check_workflow_done(slug, spec, progress_path).await;
                    // V0.4.6 F84 — budget enforcement runs last so it
                    // sees this tick's freshly-emitted agent_done /
                    // agent_spawn events. Returning `Tripped` from
                    // `enforce_budget` already wrote `budget_exceeded`
                    // + flipped workflow.yaml `enabled: false`; F82's
                    // hot-reload watcher (parallel worktree) will pick
                    // up the disabled flag and cancel the loop. Until
                    // F82 lands, the next `ccteam start` re-rosters
                    // the project and sees `enabled: false`, so the
                    // loop exits at startup (run_project early-return
                    // in F82's task list).
                    if let Err(err) = self
                        .enforce_budget(slug, spec, project_dir, progress_path)
                        .await
                    {
                        tracing::warn!(?err, slug, "enforce_budget failed");
                    }
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

        // V0.6.3 F145 — squad runtime routing. The watcher tags events
        // from `<project>/.ccteam/squad/` with the route sentinel; the
        // real target role is the `<member>--*.md` filename prefix the
        // leader chose at runtime.
        if role == crate::workflow::SQUAD_ROUTE_SENTINEL {
            return self
                .handle_squad_route(slug, spec, project_dir, progress_path, &evt)
                .await;
        }

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

        // V0.6.1 F124 narrow scope — `mode: human-approval` requires
        // an explicit `plan_decision` before spawning. Park the
        // artifact event on the pending queue (capacity-permitting)
        // and emit `plan_decision_required`; F98 plan-approval's IM
        // round-trip writes a `spawn_requests/*.json` marker on
        // APPROVE which `check_spawn_requests` consumes on the next
        // tick. REJECT drops the queued event without spawning.
        if matches!(spec.mode, WorkflowMode::HumanApproval) {
            self.pending
                .lock()
                .await
                .entry(role.clone())
                .or_default()
                .push_back(evt);
            let pending_count = self
                .pending
                .lock()
                .await
                .get(&role)
                .map(|q| q.len())
                .unwrap_or(0);
            let _ = progress::append_event(
                progress_path,
                &json!({
                    "event": "plan_decision_required",
                    "role": role,
                    "slug": slug,
                    "pending_count": pending_count,
                    "reason": "mode: human-approval — artifact-triggered spawn requires APPROVE",
                    "ts": Utc::now().to_rfc3339(),
                }),
            );
            return Ok(());
        }

        self.try_spawn(slug, &role, agent, project_dir, progress_path)
            .await
    }

    /// V0.6.3 F145 — dispatch a squad routing artifact.
    ///
    /// The leader writes `<member>--*.md` (optionally `<member>--h<N>--*`
    /// for a re-route) into `<project>/.ccteam/squad/`. This method:
    ///
    /// 1. parses the `(target_role, hop)` pair from the filename prefix
    ///    (no file-body parsing — CLAUDE.md 红线 R3);
    /// 2. rejects a prefix that is not a declared `squad.members` role
    ///    (the static membership set is the auditable allow-list);
    /// 3. enforces the [`SquadSpec::hop_limit`] depth bound — a routed
    ///    file whose hop count has reached the limit emits an
    ///    `escalation` event instead of spawning (CLAUDE.md 红线 R7);
    /// 4. otherwise spawns the target member through the normal
    ///    `try_spawn` path (`agent_spawn` → `progress.jsonl`).
    async fn handle_squad_route(
        &self,
        slug: &str,
        spec: &WorkflowSpec,
        project_dir: &std::path::Path,
        progress_path: &std::path::Path,
        evt: &ArtifactEvent,
    ) -> Result<()> {
        let Some(squad) = spec.squad.as_ref() else {
            // Watcher tagged a sentinel event but the spec has no
            // squad block — should not happen (the watcher only
            // registers the squad root when `spec.squad.is_some()`).
            tracing::warn!(slug, "squad route event but workflow declares no squad");
            return Ok(());
        };

        let file_name = evt
            .artifact_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default();

        let Some((target, hop)) = crate::workflow::parse_squad_route(file_name) else {
            // Not a routed artifact (no `<member>--` prefix) — ignore.
            tracing::debug!(
                slug,
                file = file_name,
                "squad route: filename has no `<member>--` prefix; ignoring"
            );
            return Ok(());
        };

        progress::append_event(
            progress_path,
            &json!({
                "event": "artifact_received",
                "role": crate::workflow::SQUAD_ROUTE_SENTINEL,
                "squad_target": target,
                "hop": hop,
                "artifact_path": evt.artifact_path,
                "slug": slug,
                "ts": Utc::now().to_rfc3339(),
            }),
        )?;

        // Rule 2 — reject a prefix that is not a declared member. The
        // member set is validated against `agents` at workflow load, so
        // a member that passes here is guaranteed to resolve below.
        if !squad.is_member(&target) {
            progress::append_event(
                progress_path,
                &json!({
                    "event": "escalation",
                    "kind": "squad_unknown_target",
                    "squad_target": target,
                    "artifact_path": evt.artifact_path,
                    "slug": slug,
                    "ts": Utc::now().to_rfc3339(),
                }),
            )?;
            self.send_btw_escalation(
                slug,
                &format!(
                    "squad routing artifact `{file_name}` names target `{target}` which is not \
                     a declared squad member ({:?}); dropped.",
                    squad.members,
                ),
            )
            .await;
            return Ok(());
        }

        // Rule 3 — depth bound. A routed file at hop >= hop_limit
        // escalates instead of spawning, so a leader→member→leader
        // cycle is bounded (CLAUDE.md 红线 R7).
        if hop >= squad.hop_limit {
            progress::append_event(
                progress_path,
                &json!({
                    "event": "escalation",
                    "kind": "squad_hop_limit",
                    "squad_target": target,
                    "hop": hop,
                    "hop_limit": squad.hop_limit,
                    "artifact_path": evt.artifact_path,
                    "slug": slug,
                    "ts": Utc::now().to_rfc3339(),
                }),
            )?;
            self.send_btw_escalation(
                slug,
                &format!(
                    "squad routing depth limit reached: artifact `{file_name}` is at hop {hop} \
                     (limit {}); target `{target}` not spawned. A leader→member→leader cycle \
                     is escalating per the fix-loop 3-strike rule.",
                    squad.hop_limit,
                ),
            )
            .await;
            return Ok(());
        }

        let Some(agent) = spec.agents.get(&target) else {
            // Unreachable given load-time validation, but degrade
            // gracefully rather than panic on a hot-reload race.
            tracing::warn!(
                slug,
                target = target.as_str(),
                "squad route: member passed validation but missing from agents map"
            );
            return Ok(());
        };

        tracing::info!(
            slug,
            target = target.as_str(),
            hop,
            "squad route: dispatching member"
        );

        let max_par = agent.parallelism.unwrap_or(1).max(1) as usize;
        let running_count = self
            .running
            .lock()
            .await
            .get(&target)
            .map(|v| v.len())
            .unwrap_or(0);
        if running_count >= max_par {
            // Reuse the standard pending-queue back-pressure path — a
            // squad-routed event waits its turn just like a watch event.
            self.pending
                .lock()
                .await
                .entry(target.clone())
                .or_default()
                .push_back(evt.clone());
            return Ok(());
        }

        self.try_spawn(slug, &target, agent, project_dir, progress_path)
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
        // V0.6.0 Wave 3 F112 §C — vendor_fallback hook. When the
        // cumulative Claude cost trips the cap AND the user opted in
        // via `~/.ccteam/preferences.toml::fallback.on_claude_quota
        // = "codex"`, we DON'T hard-stop: we swap this role's adapter
        // to Codex on this spawn, keep the workflow alive, and write
        // a `budget_exceeded` event tagged `vendor: claude` +
        // `vendor_fallback_to: codex` so the audit trail still shows
        // the cap. The original V0.5.x hard-stop path (no prefs / off /
        // role not eligible) is otherwise unchanged.
        let prefs = preferences::load_or_default(&self.paths.root);
        let mut effective_executor = agent.executor;
        if cost_so_far >= budget {
            let claude_vendor = matches!(agent.executor, Executor::Claude);
            let want_fallback = claude_vendor && prefs.codex_fallback_enabled_for(role);
            let codex_adapter_present = self.adapter_for(Executor::Codex).is_some();
            if want_fallback && codex_adapter_present {
                progress::append_event(
                    progress_path,
                    &json!({
                        "event": "budget_exceeded",
                        "role": role,
                        "cost_used_usd": cost_so_far,
                        "budget_limit_usd": budget,
                        "slug": slug,
                        "vendor": "claude",
                        "vendor_fallback_to": "codex",
                        "ts": Utc::now().to_rfc3339(),
                    }),
                )?;
                tracing::warn!(
                    role,
                    cost_so_far,
                    budget,
                    "claude quota tripped; swapping to codex for this spawn (prefs opt-in)"
                );
                effective_executor = Executor::Codex;
            } else {
                progress::append_event(
                    progress_path,
                    &json!({
                        "event": "budget_exceeded",
                        "role": role,
                        "cost_used_usd": cost_so_far,
                        "budget_limit_usd": budget,
                        "slug": slug,
                        "vendor": match agent.executor {
                            Executor::Claude => "claude",
                            Executor::Codex => "codex",
                        },
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
        }

        let Some(adapter) = self.adapter_for(effective_executor) else {
            tracing::warn!(role, executor = ?effective_executor, "no adapter registered");
            self.bump_fail_count(slug, role, progress_path).await?;
            return Ok(());
        };

        let sid = format!("{}-{}", role, self.next_role_seq().await);
        let raw_kick = prompt.unwrap_or_else(|| Self::DEFAULT_KICK_PROMPT.to_string());
        // V0.6.0 F115 — expand `{{include_prev_handoffs}}` + other
        // spawn-brief tokens. Hot path (no `{{` in prompt) is a cheap
        // string clone; the disk read inside `read_concat` only fires
        // when the directive is present. Failures (corrupt handoff
        // dir, IO error) fall back to the raw kick so a single bad
        // handoff doc never wedges spawn.
        let kick = {
            let brief_ctx = SpawnBriefContext::new(
                project_dir.to_path_buf(),
                slug.to_string(),
                role.to_string(),
            );
            match render_spawn_brief(&raw_kick, &brief_ctx) {
                Ok(rendered) => rendered,
                Err(err) => {
                    tracing::warn!(role, ?err, "render_spawn_brief failed; using raw kick");
                    raw_kick
                }
            }
        };
        // V0.6.0 F107 — orchestrator now drives the new
        // HarnessAdapter trait (5-method thread/turn shape). Build a
        // SpawnCtx + AgentSpecBrief, call start_thread().await, then
        // translate ThreadHandle → SessionHandle for the in-memory
        // running map + state.json registry (which still expect the
        // V0.4.0 shape — zero behaviour change downstream).
        let brief = AgentSpecBrief {
            role: role.to_string(),
        };
        let ctx = SpawnCtx {
            slug: slug.to_string(),
            sid: sid.clone(),
            // V0.6.2 — `cwd` is the agent's `scope` subdirectory joined
            // onto the project root (or the root itself when `scope` is
            // unset). Scoping each spawn to the slice of the tree
            // relevant to its role keeps the fresh-1M-context window
            // (red line R3) pointed at a small subtree instead of the
            // whole repo root; Claude Code still walks *up* and loads
            // every CLAUDE.md along the way.
            cwd: agent.cwd(project_dir),
            project_dir: project_dir.to_path_buf(),
            extra_args: vec![kick],
            // Wave 4 D14 — plumb the workflow-declared model id so the
            // adapter can spawn the right model and `ccteam_cost` can
            // price against the actual model instead of the vendor
            // fallback. `None` = vendor default.
            model_id: agent.model.clone(),
            effort: None,
            // v0.8.7 W2 — workflow/bg spawn is not the gateway chat ask-path;
            // HITL is a gateway-chat feature, so this stays skip.
            permission_mode: ccteam_harness::PermissionMode::Skip,
            // v0.8.7 review-fix (R-M1) — the orchestrator/bg path is not the
            // gateway cto-gate chat session; no per-session secret applies.
            secret: String::new(),
            // v0.9.0 W3 — `ccteam-flow` is deferred/not driven by the daemon
            // (§一); it never targets a remote host.
            remote: None,
        };

        // Atomic check-and-spawn: hold the `running` lock from the
        // parallelism gate through the handle insert. dispatch_artifact's
        // pre-check is the fast-path; this is the authoritative gate that
        // prevents two concurrent dispatchers from both seeing the slot
        // free and over-committing past `agent.parallelism`. (Race observed
        // on dex-ui: two releaser markers landing ~1.5s apart bypassed the
        // pre-check and produced 2 running with parallelism=1.)
        //
        // Race-loss path bails silently — the marker file remains in
        // `.ccteam/triggers/<role>/` and the agent already-spawned for
        // the winning marker will sweep it on its next directory scan, so
        // no work is dropped.
        let mut running = self.running.lock().await;
        let max_par = agent.parallelism.unwrap_or(1).max(1) as usize;
        let running_count = running.get(role).map(|v| v.len()).unwrap_or(0);
        if running_count >= max_par {
            tracing::debug!(
                role,
                running_count,
                max_par,
                "spawn race lost: another dispatcher claimed the slot; skipping"
            );
            return Ok(());
        }

        match adapter.start_thread(&brief, &ctx).await {
            Ok(thread_handle) => {
                let handle = SessionHandle::from_thread_handle(&thread_handle, &sid);
                running
                    .entry(role.to_string())
                    .or_default()
                    .push(handle.clone());
                drop(running);
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
                        // V0.8 W3 follow-up — persist the mode-2
                        // foreground-in-mux markers so the F80
                        // stale-spawn pass can reconstruct a via_mux
                        // handle from progress.jsonl and route its
                        // liveness through the mux session lifecycle
                        // (there is no `~/.claude/jobs/<id>/state.json`
                        // for these spawns). Default `--bg` + codex
                        // rows skip both (serde via the json! literal).
                        "via_mux": handle.via_mux,
                        "mux_session": handle.mux_session,
                        "executor": match effective_executor {
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
                drop(running);
                tracing::warn!(role, ?err, "start_thread failed");
                self.bump_fail_count(slug, role, progress_path).await?;
                Ok(())
            }
        }
    }

    /// V0.6.0 F107 — translate a [`ThreadEvent`] into a `progress.jsonl`
    /// business event. Wave 1 helper used by future adapters whose
    /// `events()` stream is non-empty (Wave 2 / Wave 3); the legacy F80
    /// `claude_job::probe_job` poller in [`Self::poll_completions`] is
    /// the active driver during Wave 1 (zero behaviour change vs.
    /// V0.5.1).
    ///
    /// Translation rules (R2 SoT — never delete `progress.jsonl`
    /// write path):
    ///
    /// - [`ThreadEvent::ThreadStarted`] → noop (`agent_spawn` is
    ///   written by `try_spawn` immediately after `start_thread`).
    /// - [`ThreadEvent::TurnCompleted`] → `agent_done` with
    ///   `status="completed"` + `cost_usd = usage_to_cost(usage,
    ///   vendor)`.
    /// - [`ThreadEvent::TurnFailed`] / [`ThreadEvent::Error`] →
    ///   `agent_done` with `status="errored"` + `error=err.message`.
    /// - `Item*` → Wave 1 noop (Wave 2 will surface `chat_*` event
    ///   types).
    #[allow(dead_code)] // Wave 2 / Wave 3 will activate this driver.
    pub(crate) fn translate_thread_event(
        evt: &ThreadEvent,
        role: &str,
        sid: &str,
        slug: &str,
        vendor: AgentVendor,
        // Wave 4 D14 — the concrete spawn-ctx model id (from
        // `SpawnCtx::model_id`), used only when the turn carries no
        // canonical model of its own. An unknown / absent model prices to
        // `None` → `cost_usd` is omitted (excluded), never a fallback rate.
        model: Option<&str>,
    ) -> Option<serde_json::Value> {
        let vendor_str = match vendor {
            AgentVendor::Claude => "claude",
            AgentVendor::Codex => "codex",
            AgentVendor::Grok => "grok",
            AgentVendor::Opencode => "opencode",
            AgentVendor::Kimi => "kimi",
            AgentVendor::Pi => "pi",
            AgentVendor::Dsh => "dsh",
        };
        match evt {
            ThreadEvent::ThreadStarted { .. } => None,
            ThreadEvent::TurnCompleted {
                usage,
                model: turn_model,
                ..
            } => {
                // Prefer the turn's own canonical model; fall back to the
                // spawn-ctx model. Determinism: an unknown / absent model
                // prices to `None` → `cost_usd` is OMITTED (excluded from
                // sums), never billed at a fallback rate.
                let priced_model = turn_model.as_deref().or(model).unwrap_or("");
                let cost = ccteam_cost::estimate_cost(
                    usage,
                    match vendor {
                        AgentVendor::Claude => ccteam_cost::Vendor::Claude,
                        AgentVendor::Codex => ccteam_cost::Vendor::Codex,
                        AgentVendor::Grok => ccteam_cost::Vendor::Grok,
                        AgentVendor::Opencode => ccteam_cost::Vendor::Opencode,
                        AgentVendor::Kimi => ccteam_cost::Vendor::Kimi,
                        AgentVendor::Pi => ccteam_cost::Vendor::Pi,
                        AgentVendor::Dsh => ccteam_cost::Vendor::Dsh,
                    },
                    priced_model,
                );
                let mut row = json!({
                    "event": "agent_done",
                    "role": role,
                    "session_id": sid,
                    "status": "completed",
                    "vendor": vendor_str,
                    "slug": slug,
                    "ts": Utc::now().to_rfc3339(),
                });
                if let Some(cost) = cost {
                    row["cost_usd"] = json!(cost);
                }
                Some(row)
            }
            ThreadEvent::TurnFailed { err, .. } => Some(json!({
                "event": "agent_done",
                "role": role,
                "session_id": sid,
                "status": "errored",
                "error": err.message,
                "vendor": vendor_str,
                "slug": slug,
                "ts": Utc::now().to_rfc3339(),
            })),
            ThreadEvent::Error(err) => Some(json!({
                "event": "agent_done",
                "role": role,
                "session_id": sid,
                "status": "errored",
                "error": err.message,
                "vendor": vendor_str,
                "slug": slug,
                "ts": Utc::now().to_rfc3339(),
            })),
            ThreadEvent::TurnStarted { .. }
            | ThreadEvent::ItemStarted { .. }
            | ThreadEvent::ItemUpdated { .. }
            | ThreadEvent::ItemCompleted { .. } => None,
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
            // V0.6.0 F115 — attach the most recent handoff trace so the
            // meta-agent / web UI can answer "why did it loop". We look
            // up the project dir for this slug (and degrade gracefully
            // when it can't be resolved or no handoffs exist).
            let project_dir = self.paths.project_dir(slug);
            let trace = handoff::read_concat(&project_dir, slug, handoff::DEFAULT_INCLUDE_LAST_N)
                .unwrap_or_default();
            let mut body = format!(
                "role `{}` failed to spawn {} consecutive times — orchestrator escalating \
                 per fix-loop 3-strike rule.",
                role, cur
            );
            if !trace.is_empty() {
                body.push_str("\n\n---\n## Recent handoff trace\n\n");
                body.push_str(&trace);
            }
            self.send_btw_escalation(slug, &body).await;
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
        // Snapshot in-memory sids BEFORE the cleanup loop empties them
        // — otherwise the stale-spawn pass below would treat sessions
        // we're about to write `agent_done` for as "untracked" and emit
        // a duplicate synthetic done, double-draining pending and
        // over-committing past `agent.parallelism`.
        let in_memory_sids: std::collections::HashSet<String> = {
            let running = self.running.lock().await;
            running
                .values()
                .flat_map(|v| v.iter().map(|h| h.sid.clone()))
                .collect()
        };
        // V0.8 W3 follow-up — mode-2 foreground-in-mux bg spawns
        // (`via_mux`) resolve liveness via an async `ProcessBackend::exists`
        // probe, not the sync state.json read. To preserve the prior
        // locking discipline (the in-lock sweep below must NOT `.await`
        // — every other `self.running` caller would stall on a
        // backend fork/RPC otherwise), we PRE-COMPUTE via_mux liveness
        // OUTSIDE the lock: snapshot the (sid, mux_session) pairs under a
        // brief guard, probe each unlocked, then consult the resulting
        // map during the synchronous sweep.
        let via_mux_probes: Vec<(String, String)> = {
            let running = self.running.lock().await;
            running
                .values()
                .flat_map(|v| v.iter())
                .filter(|h| h.via_mux)
                .map(|h| {
                    let session = h
                        .mux_session
                        .as_deref()
                        .filter(|s| !s.is_empty())
                        .unwrap_or(h.sid.as_str())
                        .to_string();
                    (h.sid.clone(), session)
                })
                .collect()
        };
        let mut via_mux_status: HashMap<String, SessionStatus> = HashMap::new();
        if !via_mux_probes.is_empty() {
            let backend = ccteam_harness::default_backend();
            for (sid, session) in via_mux_probes {
                via_mux_status.insert(
                    sid,
                    via_mux_session_status(backend.as_ref(), &session).await,
                );
            }
        }
        {
            let mut running = self.running.lock().await;
            for (role, handles) in running.iter_mut() {
                let mut keep = Vec::with_capacity(handles.len());
                for handle in handles.drain(..) {
                    // via_mux handles consult the pre-computed map (probed
                    // outside this lock); all others take the sync
                    // state.json path. A via_mux handle spawned AFTER the
                    // snapshot above (not in the map) defaults to Running
                    // and is resolved on the next tick — never retired
                    // prematurely.
                    let status = if handle.via_mux {
                        match via_mux_status.get(&handle.sid) {
                            Some(SessionStatus::Done { cost_usd, status }) => SessionStatus::Done {
                                cost_usd: *cost_usd,
                                status: status.clone(),
                            },
                            _ => SessionStatus::Running,
                        }
                    } else {
                        self.session_status(&handle)
                    };
                    match status {
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
        for open in progress::open_agent_spawns_detailed(&events) {
            let progress::OpenAgentSpawn {
                session_id: sid,
                job_id,
                role,
                via_mux,
                mux_session,
            } = open;
            if in_memory_sids.contains(&sid) {
                continue; // genuinely tracked by this orchestrator instance
                          // (snapshot taken before in-memory cleanup, so
                          // sessions transitioning to Done in this tick
                          // are still excluded)
            }
            // V0.8 W3 follow-up — mode-2 foreground-in-mux spawns
            // (`via_mux`) have NO `~/.claude/jobs/<id>/state.json`;
            // probing it would (correctly, for a missing file) return
            // `Terminal{killed}` and PREMATURELY retire a still-running
            // mux session. Route their liveness through the mux session
            // lifecycle instead, mirroring the in-memory `session_status`
            // branch. The default `--bg` path falls through to the F80
            // state.json probe unchanged.
            let (status, cost_usd) = if via_mux {
                let session = mux_session
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .unwrap_or(sid.as_str());
                let backend = ccteam_harness::default_backend();
                match via_mux_session_status(backend.as_ref(), session).await {
                    SessionStatus::Running => continue,
                    SessionStatus::Done { cost_usd, status } => (status, cost_usd.unwrap_or(0.0)),
                }
            } else {
                let verdict = ccteam_core::claude_job::probe_job(job_id.as_deref());
                let ccteam_core::claude_job::JobLiveness::Terminal { status, cost_usd } = verdict
                else {
                    continue;
                };
                (status.to_string(), cost_usd)
            };
            tracing::info!(
                slug,
                role = role.as_str(),
                session_id = sid.as_str(),
                ?job_id,
                via_mux,
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
                    via_mux,
                    mux_session,
                },
                Some(cost_usd),
                status,
            ));
        }

        for (role, handle, cost_usd, status) in finished {
            if let Some(c) = cost_usd {
                *self.cost_accum.lock().await += c;
                // V0.4.6 F91 — `state.cost_used_usd` is deprecated; the
                // cost SoT is now the `agent_done` event below plus the
                // live `cost_summary` API (which reads progress.jsonl +
                // `~/.claude/jobs/<id>/state.json`). The pre-F91 F80
                // bump is retired so we don't keep mutating a frozen
                // field. `cost_accum` (in-memory tick budget) is still
                // updated for legacy budget checks that read it.
            }
            let _ = progress::append_event(
                progress_path,
                &json!({
                    "event": "agent_done",
                    "role": role,
                    "session_id": handle.sid,
                    "status": status,
                    "cost_usd": cost_usd.unwrap_or(0.0),
                    "vendor": vendor_from_harness(&handle.harness),
                    "slug": slug,
                    "ts": Utc::now().to_rfc3339(),
                }),
            );

            if status == "completed" || status == "stopped" {
                self.fail_counts.lock().await.insert(role.clone(), 0);
            }

            // V0.6.1 F124 narrow scope — `mode: human-approval` gates
            // the pending-drain on an explicit `plan_decision` event.
            // Skip the auto-drain here and emit
            // `plan_decision_required` so the F98 plan-approval IM
            // path can prompt the user. When APPROVE lands, F98 writes
            // a `plan_decision` event + a `spawn_requests/*.json`
            // marker that `check_spawn_requests` picks up on the next
            // tick. The `pending` queue stays intact across the gate
            // so REJECT can drop the queued event without spawning.
            if matches!(spec.mode, WorkflowMode::HumanApproval) {
                let pending_count = self
                    .pending
                    .lock()
                    .await
                    .get(&role)
                    .map(|q| q.len())
                    .unwrap_or(0);
                let _ = progress::append_event(
                    progress_path,
                    &json!({
                        "event": "plan_decision_required",
                        "role": role,
                        "slug": slug,
                        "pending_count": pending_count,
                        "reason": "mode: human-approval — next step requires APPROVE",
                        "ts": Utc::now().to_rfc3339(),
                    }),
                );
                continue;
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

    /// V0.6.3 F142 — fire `trigger: schedule` agents whose cron
    /// expression is due.
    ///
    /// ## skip-missed semantics
    ///
    /// Each `(project, role)` pair has a `last_fire` timestamp
    /// persisted in `<project>/.ccteam/state.json::schedule_last_fire`.
    /// Per tick, for every schedule agent:
    ///
    /// - **first observation** (`last_fire` absent) — record
    ///   `last_fire = now` and do NOT spawn. The next occurrence is
    ///   then computed forward from daemon start, so a cold start
    ///   never fires on boot.
    /// - **subsequent ticks** — fire iff
    ///   [`ccteam_core::cron::Schedule::is_due`] returns `true` (the next
    ///   cron slot after `last_fire` is `<= now`). On fire, advance
    ///   `last_fire` to `now` — NOT to the missed slot — so a daemon
    ///   that was down across many slots fires exactly once on
    ///   restart-after-due, then resumes the normal cadence. No
    ///   backfill, no restart storm.
    ///
    /// `parallelism` is forced to 1: `try_spawn`'s `running`-map gate
    /// already caps schedule agents at one live session (validate()
    /// rejects `parallelism > 1` for non-watch triggers), so a slow
    /// agent that overruns its next cron slot simply skips that slot.
    ///
    /// `progress.jsonl` stays the SoT — the spawn emits `agent_spawn`
    /// through the normal `try_spawn` path; `state.json` only carries
    /// the scheduler's bookkeeping cursor.
    async fn check_schedules(
        &self,
        slug: &str,
        spec: &WorkflowSpec,
        project_dir: &std::path::Path,
        progress_path: &std::path::Path,
    ) {
        // Fast path: no schedule agents → no state.json IO at all.
        let has_schedule = spec
            .agents
            .values()
            .any(|a| matches!(a.trigger, Trigger::Schedule));
        if !has_schedule {
            return;
        }

        let state_path = ccteam_core::paths::CcteamPaths::project_state_in(project_dir);
        let mut state = match ccteam_core::state::ProjectState::load(&state_path) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(
                    slug,
                    ?err,
                    "F140 check_schedules: state.json load failed; skipping this tick"
                );
                return;
            }
        };

        let now = Utc::now();
        let mut state_dirty = false;
        // Roles to spawn after we drop nothing — `try_spawn` is async
        // and takes `&self`; collect first so the borrow of `state` is
        // released cleanly before each spawn.
        let mut due_roles: Vec<String> = Vec::new();

        for (role, agent) in &spec.agents {
            if !matches!(agent.trigger, Trigger::Schedule) {
                continue;
            }
            // workflow.yaml validate() guarantees a parseable 5-field
            // cron for every schedule agent; re-parse defensively so a
            // hot-reload race that slipped a bad spec past us only
            // skips this role rather than panicking the loop.
            let Some(expr) = agent.schedule.as_deref() else {
                continue;
            };
            let sched = match ccteam_core::cron::Schedule::parse(expr) {
                Ok(s) => s,
                Err(err) => {
                    tracing::warn!(
                        slug,
                        role = role.as_str(),
                        ?err,
                        "F140 check_schedules: unparseable schedule; skipping role"
                    );
                    continue;
                }
            };

            match state.schedule_last_fire.get(role).copied() {
                None => {
                    // First observation — seed the cursor, do not fire.
                    state.schedule_last_fire.insert(role.clone(), now);
                    state_dirty = true;
                    tracing::info!(
                        slug,
                        role = role.as_str(),
                        schedule = expr,
                        "F140 schedule registered; cursor anchored to now"
                    );
                }
                Some(last_fire) => {
                    if sched.is_due(Some(last_fire), now) {
                        // Advance the cursor to `now` (skip-missed) and
                        // queue the spawn.
                        state.schedule_last_fire.insert(role.clone(), now);
                        state_dirty = true;
                        due_roles.push(role.clone());
                    }
                }
            }
        }

        // Persist the cursor BEFORE spawning so a crash between the
        // save and the spawn errs on the side of skip-missed (the
        // missed slot is dropped) rather than a double-fire.
        if state_dirty {
            if let Err(err) = state.save(&state_path) {
                tracing::warn!(
                    slug,
                    ?err,
                    "F140 check_schedules: state.json save failed; \
                     schedule cursors not advanced this tick"
                );
                // Bail without spawning — next tick retries cleanly.
                return;
            }
        }

        for role in due_roles {
            let Some(agent) = spec.agents.get(&role) else {
                continue;
            };
            tracing::info!(slug, role = role.as_str(), "F140 schedule due; spawning");
            if let Err(err) = self
                .try_spawn(slug, &role, agent, project_dir, progress_path)
                .await
            {
                tracing::warn!(
                    slug,
                    role = role.as_str(),
                    ?err,
                    "F140 schedule spawn failed"
                );
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

    /// Synchronous state.json liveness probe for the default
    /// self-detaching `claude --bg` (+ codex) path. Mode-2
    /// foreground-in-mux spawns (`via_mux`) do NOT route through here —
    /// they have no `~/.claude/jobs/<id>/state.json`; their liveness is
    /// pre-computed via [`via_mux_session_status`] (an async
    /// `ProcessBackend::exists` probe) OUTSIDE the `self.running` lock in
    /// [`Self::poll_completions`], keeping this in-lock sweep sync.
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
        // V0.6.1 — also honor `CCTEAM_CLAUDE_JOBS_DIR` (the same env
        // that claude_job::probe_job + CLAUDE_JOBS_DIR_ENV
        // consult). Without this, hermetic host-probe scripts that set
        // CCTEAM_CLAUDE_JOBS_DIR could write fake state.json files
        // there, but the orchestrator's primary poll_completions still
        // looked under $HOME/.claude/jobs/<id>/ and saw nothing — only
        // the F80 stale-spawn fallback (which does honor the env) ever
        // detected completions. F120 host-probe surfaced this split.
        let id = handle.job_id.as_deref().unwrap_or("__no_job_id__");
        if let Ok(custom) = std::env::var(ccteam_core::CLAUDE_JOBS_DIR_ENV) {
            return PathBuf::from(custom).join(id).join("state.json");
        }
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
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

    /// V0.4.6 F84 — per-workflow budget enforcement.
    ///
    /// Reads `WorkflowSpec::budget` (no-op when `None` — `V0.4.5`
    /// behaviour preserved per PRD §F84 验收 #4). When the rolling
    /// 24h cost (`cost_summary.cost_24h_usd`) ≥ `max_cost_usd_per_24h`,
    /// or the rolling 1h spawn count ≥ `max_agent_spawns_per_hour`,
    /// emits a `budget_exceeded` event and flips
    /// `workflow.yaml::enabled: false` so the F82 hot-reload watcher
    /// cancels the event loop on next tick.
    ///
    /// **Idempotent**: a previously-disabled workflow re-reads
    /// disabled and the orchestrator never reaches this call (loop
    /// exits at `run_project`). If the user manually re-enables but
    /// the 24h window still exceeds the cap (PRD F84 验收 #2), the
    /// next tick re-trips — also writes a fresh `budget_exceeded`
    /// event for audit.
    ///
    /// Returns `Ok(true)` when a cap tripped this tick, `Ok(false)`
    /// otherwise. The caller (event_loop) doesn't act on the return
    /// value; the hot-reload watcher reacts to the disabled flag.
    pub async fn enforce_budget(
        &self,
        slug: &str,
        spec: &WorkflowSpec,
        project_dir: &std::path::Path,
        progress_path: &std::path::Path,
    ) -> Result<bool> {
        // F84 stub of F91 — read progress.jsonl once, derive both
        // 24h cost and 1h spawn count from the same in-memory slice.
        let events = progress::read_all_events(progress_path).unwrap_or_default();
        let cost = queries::cost_summary_from_events(&events)?;

        // V0.6.0 Wave 3 F112 — per-vendor caps (preferred). Checked
        // first so a vendor-specific overrun trips an auto-disable
        // even if the legacy flat cap is unset.
        if let Some(budgets) = &spec.budgets_v060 {
            for (vendor_key, cap_opt) in [
                ("claude", budgets.claude.max_cost_usd_per_24h),
                ("codex", budgets.codex.max_cost_usd_per_24h),
            ] {
                let Some(cap) = cap_opt else { continue };
                let value = cost
                    .cost_24h_by_vendor
                    .get(vendor_key)
                    .copied()
                    .unwrap_or(0.0);
                if value >= cap {
                    progress::append_event(
                        progress_path,
                        &json!({
                            "event": "budget_exceeded",
                            "slug": slug,
                            "kind": "cost_24h_per_vendor",
                            "vendor": vendor_key,
                            "value": value,
                            "cap": cap,
                            "ts": Utc::now().to_rfc3339(),
                        }),
                    )?;
                    tracing::warn!(
                        slug,
                        vendor = vendor_key,
                        value,
                        cap,
                        "per-vendor budget cap tripped; auto-disabling workflow"
                    );
                    self.auto_disable_workflow(
                        slug,
                        "budget_exceeded_per_vendor",
                        project_dir,
                        progress_path,
                    )
                    .await?;
                    return Ok(true);
                }
            }
        }

        let Some(budget) = &spec.budget else {
            return Ok(false);
        };

        if let Some(cap) = budget.max_cost_usd_per_24h {
            if cost.cost_24h_usd >= cap {
                progress::append_event(
                    progress_path,
                    &json!({
                        "event": "budget_exceeded",
                        "slug": slug,
                        "kind": "cost_24h",
                        "value": cost.cost_24h_usd,
                        "cap": cap,
                        "ts": Utc::now().to_rfc3339(),
                    }),
                )?;
                tracing::warn!(
                    slug,
                    cost_24h = cost.cost_24h_usd,
                    cap,
                    "budget cap tripped (cost_24h); auto-disabling workflow"
                );
                self.auto_disable_workflow(slug, "budget_exceeded", project_dir, progress_path)
                    .await?;
                return Ok(true);
            }
        }

        if let Some(rate_cap) = budget.max_agent_spawns_per_hour {
            let recent_spawns =
                queries::count_agent_spawns_within(&events, chrono::Duration::hours(1));
            if recent_spawns >= rate_cap {
                progress::append_event(
                    progress_path,
                    &json!({
                        "event": "budget_exceeded",
                        "slug": slug,
                        "kind": "spawn_rate",
                        "value": recent_spawns,
                        "cap": rate_cap,
                        "ts": Utc::now().to_rfc3339(),
                    }),
                )?;
                tracing::warn!(
                    slug,
                    recent_spawns,
                    cap = rate_cap,
                    "budget cap tripped (spawn_rate); auto-disabling workflow"
                );
                self.auto_disable_workflow(slug, "spawn_rate_exceeded", project_dir, progress_path)
                    .await?;
                return Ok(true);
            }
        }

        Ok(false)
    }

    /// V0.4.6 F84 — flip `workflow.yaml::enabled: false` + write a
    /// `workflow_done reason=<reason>` event.
    ///
    /// **F82 stub**: F82's hot-reload watcher (parallel worktree) is
    /// the real consumer — it observes the mtime change, cancels the
    /// event loop, and the next roster scan sees `enabled: false` and
    /// skips the project. Until F82 lands, the disabled flag is still
    /// honoured on next `ccteam start` (run_project early-return
    /// added by F82). The `workflow_done` event we emit here is the
    /// audit trail for the budget trip; F82 may emit a second
    /// `workflow_done reason="disabled"` when the watcher fires — both
    /// are fine for the meta-agent / UI.
    ///
    /// Uses simple text-level mutation (`enabled: false` line insert
    /// / replace) instead of full YAML round-trip via serde_yaml: the
    /// latter rewrites the file with serde's normalised form, which
    /// would clobber user comments + ordering. The mutation is
    /// idempotent — already-disabled file is left untouched.
    pub async fn auto_disable_workflow(
        &self,
        slug: &str,
        reason: &str,
        project_dir: &std::path::Path,
        progress_path: &std::path::Path,
    ) -> Result<()> {
        let workflow_path = workflow_yaml_path_for(project_dir);
        if let Some(path) = workflow_path {
            if let Err(err) = patch_workflow_yaml_enabled_false(&path) {
                tracing::warn!(
                    slug,
                    path = %path.display(),
                    ?err,
                    "auto_disable_workflow: yaml patch failed; will still emit workflow_done"
                );
            }
        } else {
            tracing::warn!(
                slug,
                "auto_disable_workflow: no workflow.yaml found in project_dir; emitting event only"
            );
        }

        progress::append_event(
            progress_path,
            &json!({
                "event": "workflow_done",
                "slug": slug,
                "reason": reason,
                "ts": Utc::now().to_rfc3339(),
            }),
        )?;
        Ok(())
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

    /// V0.6.3 F142 — drive one `check_schedules` pass (cron tick).
    pub async fn test_check_schedules(
        &self,
        slug: &str,
        spec: &WorkflowSpec,
        project_dir: &std::path::Path,
        progress_path: &std::path::Path,
    ) {
        self.check_schedules(slug, spec, project_dir, progress_path)
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

    // ---- V0.4.6 F82 test hooks ----------------------------------------

    /// Test seam: register a `cancel_tx` against a slug without going
    /// through the real `spawn_new_rostered_projects` flow. Used by
    /// `workflow_enabled_test.rs` to simulate a "loop is running"
    /// state before exercising `unroster_project` / `reload_project`.
    pub async fn test_cancel_handles_insert(&self, slug: &str, tx: oneshot::Sender<CancelReason>) {
        self.cancel_handles
            .lock()
            .await
            .insert(slug.to_string(), tx);
    }

    /// Test seam: mark a slug as already-spawned so `reload_project`
    /// has something to remove.
    pub async fn test_spawned_insert(&self, slug: &str) {
        self.spawned.lock().await.insert(slug.to_string());
    }

    /// Test seam: read the `spawned` set membership.
    pub async fn test_spawned_contains(&self, slug: &str) -> bool {
        self.spawned.lock().await.contains(slug)
    }
}

#[cfg(test)]
mod via_mux_tests {
    //! V0.8 W3 follow-up — the orchestrator routes mode-2
    //! foreground-in-mux bg spawns' completion through the mux session
    //! lifecycle instead of the F80 `state.json` probe. These tests
    //! exercise the [`via_mux_session_status`] helper directly against a
    //! `InProcBackend` instance we control (spawn → exists==true →
    //! Running; kill → exists==false → Done), with no real mux daemon
    //! and no env mutation (the helper takes the backend by reference).

    use std::sync::Arc;

    use ccteam_harness::{InProcBackend, MuxSessionId, MuxSessionSpec, ProcessBackend};

    use super::{via_mux_session_status, SessionStatus};

    fn spec(name: &str) -> MuxSessionSpec {
        MuxSessionSpec::new(
            name.to_string(),
            vec!["true".to_string()],
            std::env::temp_dir(),
        )
    }

    /// A live mux session (process still running the agent) reports
    /// `Running` — the orchestrator must NOT retire it.
    #[tokio::test]
    async fn live_mux_session_reports_running() {
        let backend: Arc<dyn ProcessBackend> = Arc::new(InProcBackend::new());
        let name = "ccteam-bg-demo-sid1";
        backend.spawn(spec(name)).await.unwrap();

        match via_mux_session_status(backend.as_ref(), name).await {
            SessionStatus::Running => {}
            SessionStatus::Done { .. } => panic!("live via_mux session must report Running"),
        }
    }

    /// Once the `claude -p` foreground run exits the mux session is
    /// gone (`exists == false`); the orchestrator emits a synthetic
    /// `agent_done` with `status = "completed"` — the same shape the
    /// state.json path emits for a clean finish.
    #[tokio::test]
    async fn gone_mux_session_reports_completed() {
        let backend: Arc<dyn ProcessBackend> = Arc::new(InProcBackend::new());
        let name = "ccteam-bg-demo-sid2";
        backend.spawn(spec(name)).await.unwrap();
        backend
            .kill(&MuxSessionId::new(name.to_string()))
            .await
            .unwrap();

        match via_mux_session_status(backend.as_ref(), name).await {
            SessionStatus::Done { status, cost_usd } => {
                assert_eq!(status, "completed");
                // Cost rollup for via_mux spawns is deferred to the
                // RmuxBackend bring-up wave (no per-job state.json yet).
                assert_eq!(cost_usd, None);
            }
            SessionStatus::Running => panic!("exited via_mux session must report Done"),
        }
    }

    /// A session that was never spawned (e.g. F80 reconstructs a handle
    /// from progress.jsonl after the run already finished) reports
    /// `Done` — `exists == false` is the done signal.
    #[tokio::test]
    async fn unknown_mux_session_reports_completed() {
        let backend: Arc<dyn ProcessBackend> = Arc::new(InProcBackend::new());
        match via_mux_session_status(backend.as_ref(), "ccteam-bg-never-spawned").await {
            SessionStatus::Done { status, .. } => assert_eq!(status, "completed"),
            SessionStatus::Running => panic!("nonexistent via_mux session must report Done"),
        }
    }
}
