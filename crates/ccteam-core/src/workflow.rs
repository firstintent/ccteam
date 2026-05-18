//! V0.4.0 F63 — `workflow.yaml` schema + parser.
//!
//! ## Scope
//!
//! Pure data layer: parse `workflow.yaml` into a typed [`WorkflowSpec`],
//! validate structural rules, expose role → [`AgentSpec`] map preserving
//! YAML declaration order (so logs / fixtures / trigger graph build are
//! deterministic). **No IO side effects** beyond reading the file in
//! [`WorkflowSpec::load`].
//!
//! ## Red lines (PRD v0-4-0 §F63)
//!
//! 1. `workflow.yaml` MUST NOT contain `prompt:`, `system_prompt:`, or
//!    `messages:` fields. Prompts live in `.claude/agents/<role>.md`.
//!    Any future PR adding a prompt field here is a schema violation.
//! 2. This module is **team-name agnostic**: no string literals like
//!    `"dev"`, `"qa"`, `"ccteam"` outside test fixtures.
//! 3. Parser writes nothing to `progress.jsonl`. Pure parse + validate.
//!
//! ## Trigger forms (YAML scalar string)
//!
//! - `manual`            — explicit `ccteam trigger <role>` invocation.
//! - `schedule`          — periodic; V0.4.0 stub (meta-agent triggers
//!                         manually); V0.4.1 will wire `interval` cron.
//! - `gate`              — wait until `trigger_gate` MCP tool releases.
//! - `watch:<path>`      — inotify on artifact dir; new file → spawn.
//!
//! ## See also
//!
//! - `docs/v0-4-0/prd.md` §6.1 (full schema spec)
//! - `docs/v0-4-0/dev-plan.md` §5 (PR #4 task list)
//! - `docs/interfaces.md` §17 (workflow.yaml schema reference)

use indexmap::IndexMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::path::{Path, PathBuf};

/// Full `workflow.yaml` document.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct WorkflowSpec {
    /// Workflow identifier (unique within a project).
    pub name: String,
    /// Optional human-readable description for the meta-agent / UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// V0.5.0 F93b — workflow execution mode. `artifact-driven` (the
    /// V0.4.6 default) drives spawns from `ArtifactWatcher` + trigger
    /// graph; `agent-team` runs a single ccteam-managed `__lead`
    /// session under Anthropic's experimental Agent Teams surface
    /// (`CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1`). Defaults to
    /// `ArtifactDriven` via `#[serde(default)]` so V0.4.6 workflow.yaml
    /// files that omit the field stay backwards-compatible.
    #[serde(default, skip_serializing_if = "is_mode_default")]
    pub mode: WorkflowMode,
    /// V0.4.6 F82 — soft toggle. `false` makes the daemon skip rostering
    /// this workflow (and tear down a running loop on hot-reload). The
    /// project's `state.json`, `progress.jsonl`, and artifact dirs stay
    /// intact; flipping back to `true` resumes from where the last loop
    /// left off. Defaults to `true` for backwards compatibility with
    /// V0.4.5 workflow.yaml files that omit the field.
    ///
    /// Skipped on serialize when `true` (the default) so round-trips
    /// don't add boilerplate to hand-authored workflow.yaml files —
    /// only the opt-out form `enabled: false` shows up in the
    /// rendered YAML.
    #[serde(
        default = "default_enabled",
        skip_serializing_if = "is_enabled_default"
    )]
    pub enabled: bool,
    /// V0.4.6 F84: optional budget cap. When set, the orchestrator
    /// runs `enforce_budget` at each tick; tripping a cap emits a
    /// `budget_exceeded` event and flips `enabled: false` via the F82
    /// `auto_disable_workflow` codepath. Default `None` (no budget,
    /// V0.4.5 behaviour) keeps every existing workflow.yaml
    /// unaffected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<BudgetSpec>,
    /// V0.6.0 Wave 1 — per-vendor budget split. When `Some`, takes
    /// precedence over the V0.5 flat `budget` field; `claude` half is
    /// the V0.5-equivalent path and `codex` adds the Codex-vendor cap.
    /// `None` keeps the V0.5 flat-budget semantics (Wave 2 / V0.7
    /// deprecates `BudgetSpec`). `#[serde(default)]` so V0.5 files
    /// without this key still parse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budgets_v060: Option<ccteam_cost::Budgets>,
    /// V0.5.0 F93b — populated when `mode: agent-team`. Lead session
    /// config (`team_name`, `lead_seed`, `teammate_mode`, etc.) plus
    /// the optional declarative `suggested_teammates` list. `None` in
    /// `artifact-driven` mode (the V0.4.6 default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_team: Option<AgentTeamSpec>,
    /// Role → agent spec. `IndexMap` preserves YAML declaration order so
    /// trigger graph build is deterministic across runs.
    ///
    /// V0.5.0 F93b — empty in `mode: agent-team` workflows (the lead +
    /// teammates are runtime topology, not declarative). Schema
    /// validation allows an empty `agents` map only when
    /// `mode == AgentTeam`.
    #[serde(default)]
    pub agents: IndexMap<String, AgentSpec>,
}

/// V0.5.0 F93b — workflow execution mode discriminator.
///
/// `artifact-driven` is the V0.4.0 default: `ArtifactWatcher` drives
/// dispatch from the `Trigger::*` enum and the `agents:` map. The
/// daemon polls `state.json` for completion and writes the 7 canonical
/// events to `progress.jsonl`.
///
/// `agent-team` is V0.5.0 F93b: the orchestrator spawns a single
/// `__lead` Claude session with `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1`
/// and the lead in turn decides team composition via Anthropic's
/// native `TeamCreate` + `Task` tools. The 6 `team_*` events
/// (F95 + F94) replace the spawn/done axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum WorkflowMode {
    /// V0.4.0 default. Driven by `ArtifactWatcher` + trigger graph.
    #[default]
    ArtifactDriven,
    /// V0.5.0 F93b. Driven by a single ccteam-managed `__lead`
    /// session under Anthropic Agent Teams.
    AgentTeam,
}

/// Predicate paired with `WorkflowMode::default()` so an explicit
/// `mode: artifact-driven` is also omitted from serialized YAML —
/// only the opt-in `mode: agent-team` line is rendered.
fn is_mode_default(v: &WorkflowMode) -> bool {
    matches!(v, WorkflowMode::ArtifactDriven)
}

/// V0.5.0 F97 — `cleanup_on_stop` strategy for the agent-team mode
/// `ccteam stop <slug>` flow. F93b MVP only honored a single value
/// (`force-kill`); F97 expands the choice into a typed enum so the CLI
/// can dispatch on `Self::*`.
///
/// Default is [`Self::ForceKill`] — matches V0.4.6 behavior + the F93b
/// MVP semantics so workflow.yaml files written before F97 continue to
/// behave the same way. There is **no backwards-compat shim** for the
/// old `Option<String>` shape (CLAUDE.md §五: pre-v1.0 is dev-stage,
/// no migration shims allowed); the type change is hard.
///
/// Wire form (YAML scalar):
///
/// ```yaml
/// cleanup_on_stop: force-kill      # default; SIGKILL the lead bg job
/// cleanup_on_stop: ask-lead        # write user-turn cleanup message
/// cleanup_on_stop: leave-running   # detach watcher; keep lead alive
/// ```
///
/// Behavior dispatch lives in `crates/ccteam-cli/src/commands.rs::run_stop_slug`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum CleanupOnStop {
    /// V0.4.6 compat: SIGKILL the lead bg job + scrub
    /// `.ccteam/team-snapshot.json`. Teammates die with the lead
    /// because Anthropic's in-process / tmux mode binds them to the
    /// lead's PID tree.
    #[default]
    ForceKill,
    /// F97: write a user-turn cleanup message to
    /// `.ccteam/inbox/<ts>-stop-request.md`. The lead picks it up on
    /// the next turn, runs its native cleanup flow, writes
    /// `workflow_done` to `progress.jsonl`, and exits. ccteam waits up
    /// to `--stop-timeout` seconds (default 60) for that event; on
    /// timeout, falls back to `ForceKill` with a WARN.
    AskLead,
    /// F97: drop F95 watcher entries + clear the project's daemon-side
    /// registration, but leave the lead bg job + teammate sessions
    /// running. The user can re-attach later with
    /// `ccteam start --restart-team <slug>` or `claude attach <id>`.
    /// The project's `state.json` records `detached: true` so a
    /// subsequent plain `ccteam start <slug>` refuses with a friendly
    /// error pointing at `--restart-team`.
    LeaveRunning,
}

impl CleanupOnStop {
    /// Wire-format scalar — the same kebab-case shape `serde` uses.
    /// Used by `team-snapshot.json` serializer + log lines without
    /// going through full `serde_yaml` round-trip.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ForceKill => "force-kill",
            Self::AskLead => "ask-lead",
            Self::LeaveRunning => "leave-running",
        }
    }
}

/// Default for `WorkflowSpec::enabled` when the YAML omits the field.
/// `true` matches V0.4.5 behaviour (no field = run the workflow).
fn default_enabled() -> bool {
    true
}

/// Predicate paired with `default_enabled` so `enabled: true` (the
/// default) is omitted from serialized YAML — only the opt-out form
/// `enabled: false` is rendered.
fn is_enabled_default(v: &bool) -> bool {
    *v
}

/// V0.4.6 F84 — optional `budget` block under workflow.yaml top-level.
///
/// ```yaml
/// budget:
///   max_cost_usd_per_24h: 5.00         # rolling 24h cost cap
///   max_agent_spawns_per_hour: 100     # rolling 1h spawn-rate cap
/// ```
///
/// Both fields are optional; a missing field disables that specific
/// cap (the other still trips). A missing `budget` block entirely
/// (`None`) → no-op (PRD F84 验收 #4).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct BudgetSpec {
    /// Rolling 24h window cost cap (USD). Sum of `agent_done.cost_usd`
    /// for events with `ts >= now - 24h` ≥ this value → trip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost_usd_per_24h: Option<f64>,
    /// Rolling 1h spawn count cap. Number of `agent_spawn` events with
    /// `ts >= now - 1h` ≥ this value → trip. Defends against
    /// self-excitation runaway (e.g. explorer writing to its own watch
    /// dir — observed in dex-ui 2026-05-16 burning $1.10/4h).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_agent_spawns_per_hour: Option<u32>,
}

/// V0.5.0 F93b — populated when `WorkflowSpec::mode == AgentTeam`.
///
/// Mirrors PRD §F93b schema (`docs/v0-5-0/prd.md` lines 184-230):
///
/// ```yaml
/// agent_team:
///   team_name: flaky-test-debate     # = ~/.claude/teams/<team_name>/
///   lead_seed: |                      # user-turn message to the lead
///     Investigate why integration tests in src/auth/ flake.
///   teammate_mode: in-process         # in-process | tmux | auto
///   cleanup_on_stop: force-kill       # force-kill | ask-lead | leave-running (F97)
///   snapshot_path: .ccteam/team-snapshot.json
///   suggested_teammates: []           # optional declarative list
///   auto_spawn_teammates: false       # default — plan-first gate
/// ```
///
/// Schema red lines (CLAUDE.md §三 + PRD §F93b 红线):
/// - `team_name` is the Anthropic `~/.claude/teams/<team_name>/` dir
///   name — must match for the F95 watcher to mirror events
/// - `lead_seed` is a **user-turn message**, not a system prompt
/// - `auto_spawn_teammates: false` is the default; Plan-first Protocol
///   requires explicit user approval before the lead spawns teammates
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct AgentTeamSpec {
    /// Anthropic team name (= `~/.claude/teams/<team_name>/` dir).
    /// Must be unique under that root. Required.
    pub team_name: String,
    /// User-turn message the orchestrator writes into the lead's
    /// input pipe on spawn. NOT a system prompt — the `__lead.md`
    /// system prompt is unchanged across all workflows. CLAUDE.md §三
    /// 红线: "永不向 session 注入 system prompt".
    pub lead_seed: String,
    /// `CLAUDE_CODE_TEAMMATE_MODE` env value passed to the lead.
    /// `in-process` (default, Anthropic native) / `tmux` (each
    /// teammate in its own tmux pane) / `auto` (lead decides).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub teammate_mode: Option<String>,
    /// V0.5.0 F97 — what to do when the user runs `ccteam stop <slug>`.
    /// `force-kill` (default + V0.4.6 compat) SIGKILLs the lead;
    /// `ask-lead` writes a user-turn cleanup message to the lead's
    /// `.ccteam/inbox/` and waits for `workflow_done`; `leave-running`
    /// drops the F95 watcher entries but leaves the lead bg job +
    /// teammates alive for a later `ccteam start --restart-team`.
    /// `#[serde(default)]` keeps V0.4.6 workflow.yaml without the field
    /// loadable as `ForceKill` (no migration shim needed; CLAUDE.md §
    /// 五 "no backwards-compat shim").
    #[serde(default)]
    pub cleanup_on_stop: CleanupOnStop,
    /// V0.5.0 F93 stickiness — workflow.yaml is parsed once at spawn;
    /// the resolved spec is frozen to this snapshot path so mid-flight
    /// edits to workflow.yaml don't affect the running team. Default
    /// `.ccteam/team-snapshot.json` relative to project root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_path: Option<PathBuf>,
    /// Optional declarative teammate list. Empty / omitted = lead
    /// decides composition entirely from `lead_seed`. Present = lead
    /// follows the list (with minor adjustments allowed). See PRD
    /// "Definition-backed vs Ad-hoc 决策树".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggested_teammates: Vec<SuggestedTeammate>,
    /// Default `false` — Plan-first Protocol gates spawn on user
    /// approval. `true` = lead self-decides composition from
    /// `lead_seed` and spawns immediately (writes an audit log to
    /// `.ccteam/outbox/team-bootstrap-<ts>.md`).
    ///
    /// V0.5.0 F93b 红线: explicit `true` only; the field cannot be
    /// silently omitted to enable autonomous spawn.
    #[serde(default)]
    pub auto_spawn_teammates: bool,
}

impl AgentTeamSpec {
    /// V0.5.0 F97 — classify a workflow.yaml `agent_team:` diff into
    /// hot vs cold reload. Returns `Some(reason)` when a cold reload
    /// is required (caller emits
    /// `workflow_done reason="cold_reload_required"` + suggests
    /// `ccteam start --restart-team <slug>`); `None` means the diff
    /// is hot and the orchestrator can apply it on the next tick by
    /// writing a user-turn message to the lead's inbox.
    ///
    /// **Cold fields** (require fresh `__lead` spawn):
    /// - `team_name` — the `~/.claude/teams/<>/` dir name is baked
    ///   into the bg job at spawn time.
    /// - `suggested_teammates[].role` / `.kind` / `.spawn_brief` —
    ///   topology changes require the lead to re-plan from scratch
    ///   (Plan-first Protocol can't reliably diff a half-spawned team).
    ///
    /// **Hot fields** (apply via lead inbox on next tick):
    /// - `lead_seed` — sent as a new user-turn message.
    /// - `teammate_mode` — env-var only relevant at spawn time, but
    ///   we still classify it as hot (no point restarting just for an
    ///   env hint that's already cached on the bg job).
    /// - `suggested_teammates[].adhoc_model` / `.adhoc_color` /
    ///   `.adhoc_tools` — cosmetic / UI-only metadata.
    /// - `cleanup_on_stop` — picked up at `ccteam stop` time, not at
    ///   spawn time.
    /// - `auto_spawn_teammates` — Plan-first protocol gate is decided
    ///   on the next plan emission; hot-reload safe.
    pub fn classify_reload(&self, other: &Self) -> Option<String> {
        if self.team_name != other.team_name {
            return Some(format!(
                "team_name changed `{}` → `{}`",
                self.team_name, other.team_name,
            ));
        }
        if self.suggested_teammates.len() != other.suggested_teammates.len() {
            return Some(format!(
                "suggested_teammates count {} → {}",
                self.suggested_teammates.len(),
                other.suggested_teammates.len(),
            ));
        }
        for (a, b) in self
            .suggested_teammates
            .iter()
            .zip(other.suggested_teammates.iter())
        {
            if a.role != b.role {
                return Some(format!(
                    "suggested_teammates[].role changed `{}` → `{}`",
                    a.role, b.role,
                ));
            }
            if a.kind != b.kind {
                return Some(format!("suggested_teammates[`{}`].kind changed", a.role,));
            }
            if a.spawn_brief != b.spawn_brief {
                return Some(format!(
                    "suggested_teammates[`{}`].spawn_brief changed",
                    a.role,
                ));
            }
        }
        None
    }
}

/// V0.5.0 F93b — one entry under `agent_team.suggested_teammates`.
/// Read by `__lead` at startup; either a definition-backed role
/// (file at `.claude/agents/<role>.md`) or an ad-hoc role (entire
/// prompt inlined in `spawn_brief`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct SuggestedTeammate {
    /// Role name (Anthropic teammate `name` field). For
    /// `kind: definition`, must match the `.claude/agents/<role>.md`
    /// filename basename.
    pub role: String,
    /// Whether the teammate is backed by a `.claude/agents/<role>.md`
    /// definition or is ad-hoc (prompt inlined here).
    pub kind: SuggestedTeammateKind,
    /// Task-specific brief the lead appends to the teammate's prompt
    /// at `Task` invocation time. For `kind: definition`, this is
    /// merely the per-spawn brief (Claude already appends the .md body).
    /// For `kind: ad-hoc`, this is the entire teammate prompt
    /// (after the Worker Preamble inlined by the lead).
    pub spawn_brief: String,
    /// ad-hoc only — model id (e.g. `sonnet`, `opus`, `haiku`).
    /// Required when `kind: ad-hoc`; ignored when `kind: definition`
    /// (the .md frontmatter `model` field wins).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adhoc_model: Option<String>,
    /// ad-hoc only — UI accent color shown on the web Topology panel.
    /// Optional even for ad-hoc.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adhoc_color: Option<String>,
    /// ad-hoc only — tool list passed to `Task` (overrides lead's
    /// permission inheritance). Omitted = inherit lead's permissions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adhoc_tools: Option<Vec<String>>,
}

/// V0.5.0 F93b — `SuggestedTeammate::kind` discriminator.
///
/// `definition`: `.claude/agents/<role>.md` exists; the lead uses
/// `Task(subagent_type: "<role>", ...)` and Claude auto-appends the
/// .md body to the teammate's system prompt.
///
/// `ad-hoc`: no .md file; the lead inlines the entire teammate prompt
/// (Worker Preamble + `spawn_brief`) and calls
/// `Task(subagent_type: "general-purpose", model: "<adhoc_model>", ...)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SuggestedTeammateKind {
    /// `.claude/agents/<role>.md` exists.
    Definition,
    /// No .md file; prompt inlined in `spawn_brief`.
    AdHoc,
}

/// Per-agent role configuration.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct AgentSpec {
    /// Harness binary that runs this role. Defaults to `Claude` when the
    /// YAML field is omitted — Codex is opt-in.
    #[serde(default)]
    pub executor: Executor,
    /// What triggers a new session of this role. See [`Trigger`].
    pub trigger: Trigger,
    /// Max concurrent sessions of this role. Only meaningful for
    /// `Trigger::Watch`; validate() rejects `> 1` for other triggers.
    /// `None` semantics = "single instance" (caller treats `None` as 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallelism: Option<u32>,
    /// Optional input artifact dir (relative to project root). Passed to
    /// the spawned harness via `CCTEAM_INPUT` env var.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<PathBuf>,
    /// Optional output artifact dir (relative to project root). Passed
    /// via `CCTEAM_OUTPUT` env var.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<PathBuf>,
    /// Schedule interval (e.g. `"5m"`, `"1h"`) when
    /// `trigger == Trigger::Schedule`. Ignored otherwise. V0.4.0 keeps
    /// this as opaque string; cron / duration parsing lands in V0.4.1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<String>,
    /// Optional per-session timeout (duration string). V0.4.0 carries
    /// the field for later watchdog consumption.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
    /// What to do when `timeout` elapses. Default `Escalate`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_timeout: Option<OnTimeout>,
}

/// Harness binary executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Executor {
    /// Anthropic Claude Code CLI (default).
    #[default]
    Claude,
    /// OpenAI Codex CLI. Requires `CodexAdapter` (V0.3.1 stub; V0.4.0
    /// real impl per PRD §6.5).
    Codex,
}

/// What to do when an agent session exceeds its `timeout`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OnTimeout {
    /// Push a watchdog alert to meta-agent outbox; do not kill.
    Escalate,
    /// Tear down session and respawn with same inputs.
    Retry,
    /// Mark session abandoned; do not respawn.
    Skip,
}

/// What event spawns a new session of an agent role.
///
/// YAML scalar grammar:
///
/// ```yaml
/// trigger: manual            # explicit ccteam trigger <role>
/// trigger: schedule          # periodic (interval field + V0.4.1 cron)
/// trigger: gate              # wait for trigger_gate MCP call
/// trigger: watch:.ccteam/issues/  # inotify on path
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    /// Meta-agent / user explicitly invokes `ccteam trigger <role>`.
    Manual,
    /// Periodic. V0.4.0 = meta-agent-only manual trigger placeholder;
    /// V0.4.1 wires actual scheduler reading `AgentSpec::interval`.
    Schedule,
    /// Wait for `trigger_gate` MCP call to release. Requires `input`
    /// dir so the agent has artifacts to consume on release.
    Gate,
    /// Watch the given path for new files; each new file spawns one
    /// session (subject to `parallelism`).
    Watch(PathBuf),
}

impl<'de> Deserialize<'de> for Trigger {
    fn deserialize<D>(d: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(d)?;
        parse_trigger(&s).map_err(serde::de::Error::custom)
    }
}

impl Serialize for Trigger {
    fn serialize<S>(&self, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let rendered = match self {
            Trigger::Manual => "manual".to_string(),
            Trigger::Schedule => "schedule".to_string(),
            Trigger::Gate => "gate".to_string(),
            Trigger::Watch(path) => format!("watch:{}", path.display()),
        };
        s.serialize_str(&rendered)
    }
}

fn parse_trigger(raw: &str) -> Result<Trigger, String> {
    let trimmed = raw.trim();
    match trimmed {
        "manual" => Ok(Trigger::Manual),
        "schedule" => Ok(Trigger::Schedule),
        "gate" => Ok(Trigger::Gate),
        other => {
            if let Some(rest) = other.strip_prefix("watch:") {
                // Note: empty path (`watch:`) parses successfully here;
                // WorkflowSpec::validate() rejects it with
                // ValidationFailed so the error surfaces at the
                // workflow-level rather than as a raw parse error.
                Ok(Trigger::Watch(PathBuf::from(rest)))
            } else {
                Err(format!(
                    "unknown trigger `{other}`: expected one of \
                     `manual`, `schedule`, `gate`, or `watch:<path>`"
                ))
            }
        }
    }
}

/// Errors surfaced by [`WorkflowSpec::load`] / [`WorkflowSpec::validate`].
#[derive(thiserror::Error, Debug)]
pub enum WorkflowError {
    /// Neither `<project>/.ccteam/workflow.yaml` (canonical) nor
    /// `<project>/workflow.yaml` (legacy V0.4.0–V0.4.5 fallback) exists.
    #[error("workflow.yaml not found in {0:?}")]
    NotFound(PathBuf),
    /// Filesystem read failure (permissions, EIO, etc).
    #[error("workflow.yaml read failed: {0}")]
    ReadFailed(#[from] std::io::Error),
    /// YAML syntax error or unknown enum variant.
    #[error("workflow.yaml parse failed: {0}")]
    ParseFailed(#[from] serde_yaml::Error),
    /// Semantic validation failure (empty watch path, gate without
    /// input, illegal role name, etc.).
    #[error("workflow validation failed: {0}")]
    ValidationFailed(String),
}

impl WorkflowSpec {
    /// Parse and validate a workflow.yaml at an explicit path.
    pub fn load(path: &Path) -> Result<Self, WorkflowError> {
        let body = std::fs::read_to_string(path)?;
        let spec: WorkflowSpec = serde_yaml::from_str(&body)?;
        spec.validate()?;
        Ok(spec)
    }

    /// Discovery: probe `<project_dir>/.ccteam/workflow.yaml` first
    /// (canonical V0.4.6+ location), then fall back to
    /// `<project_dir>/workflow.yaml` (V0.4.0–V0.4.5 legacy; removed in
    /// V0.5). Returns [`WorkflowError::NotFound`] when neither exists.
    pub fn load_for_project(project_dir: &Path) -> Result<Self, WorkflowError> {
        let nested = project_dir.join(".ccteam").join("workflow.yaml");
        if nested.exists() {
            return Self::load(&nested);
        }
        let direct = project_dir.join("workflow.yaml");
        if direct.exists() {
            return Self::load(&direct);
        }
        Err(WorkflowError::NotFound(project_dir.to_path_buf()))
    }

    /// Apply structural validation (PRD §6.1):
    ///
    /// 1. `Trigger::Watch(path)` — path must be non-empty.
    /// 2. `Trigger::Gate` — `input` must be set (otherwise the gate has
    ///    nothing to hand off on release).
    /// 3. Agent role name only `[a-z0-9_-]`, length ≥ 1 (must map to a
    ///    valid `.claude/agents/<role>.md` filename).
    /// 4. `parallelism > 1` only allowed with `Trigger::Watch`;
    ///    `schedule` / `gate` / `manual` are single-instance.
    /// 5. `agents` map must be non-empty (artifact-driven only).
    /// 6. V0.5.0 F93b — `mode: agent-team` requires `agent_team` block;
    ///    `agent-team.team_name` + `lead_seed` non-empty. `agents` may
    ///    be empty in this mode (the lead drives spawn at runtime).
    pub fn validate(&self) -> Result<(), WorkflowError> {
        match self.mode {
            WorkflowMode::ArtifactDriven => {
                if self.agents.is_empty() {
                    return Err(WorkflowError::ValidationFailed(
                        "workflow must declare at least one agent".to_string(),
                    ));
                }
                if self.agent_team.is_some() {
                    return Err(WorkflowError::ValidationFailed(
                        "agent_team block is only valid when mode: agent-team".to_string(),
                    ));
                }
            }
            WorkflowMode::AgentTeam => {
                let team = self.agent_team.as_ref().ok_or_else(|| {
                    WorkflowError::ValidationFailed(
                        "mode: agent-team requires an `agent_team` block".to_string(),
                    )
                })?;
                if team.team_name.trim().is_empty() {
                    return Err(WorkflowError::ValidationFailed(
                        "agent_team.team_name must be non-empty".to_string(),
                    ));
                }
                validate_role_name(&team.team_name).map_err(|_| {
                    WorkflowError::ValidationFailed(format!(
                        "agent_team.team_name `{name}`: only [a-z0-9_-] allowed (must map to \
                         ~/.claude/teams/<team_name>/ dir)",
                        name = team.team_name,
                    ))
                })?;
                if team.lead_seed.trim().is_empty() {
                    return Err(WorkflowError::ValidationFailed(
                        "agent_team.lead_seed must be a non-empty user-turn message".to_string(),
                    ));
                }
                for t in &team.suggested_teammates {
                    validate_role_name(&t.role)?;
                    if matches!(t.kind, SuggestedTeammateKind::AdHoc) && t.adhoc_model.is_none() {
                        return Err(WorkflowError::ValidationFailed(format!(
                            "suggested_teammate `{role}`: kind: ad-hoc requires adhoc_model",
                            role = t.role,
                        )));
                    }
                }
            }
        }
        for (role, spec) in &self.agents {
            validate_role_name(role)?;
            match &spec.trigger {
                Trigger::Watch(path) => {
                    if path.as_os_str().is_empty() {
                        return Err(WorkflowError::ValidationFailed(format!(
                            "agent `{role}`: trigger `watch:` requires a non-empty path"
                        )));
                    }
                }
                Trigger::Gate => {
                    if spec.input.is_none() {
                        return Err(WorkflowError::ValidationFailed(format!(
                            "agent `{role}`: trigger `gate` requires an `input` directory"
                        )));
                    }
                }
                Trigger::Manual | Trigger::Schedule => {}
            }
            if let Some(n) = spec.parallelism {
                if n > 1 && !matches!(spec.trigger, Trigger::Watch(_)) {
                    return Err(WorkflowError::ValidationFailed(format!(
                        "agent `{role}`: parallelism > 1 only valid with `watch:` trigger \
                         (schedule / gate / manual are single-instance)"
                    )));
                }
            }
        }
        Ok(())
    }
}

fn validate_role_name(role: &str) -> Result<(), WorkflowError> {
    if role.is_empty() {
        return Err(WorkflowError::ValidationFailed(
            "agent role name must be non-empty".to_string(),
        ));
    }
    for ch in role.chars() {
        let ok = ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-';
        if !ok {
            return Err(WorkflowError::ValidationFailed(format!(
                "agent role `{role}`: character `{ch}` not allowed \
                 (only [a-z0-9_-] permitted)"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_trigger_manual() {
        assert_eq!(parse_trigger("manual").unwrap(), Trigger::Manual);
    }

    #[test]
    fn parse_trigger_schedule() {
        assert_eq!(parse_trigger("schedule").unwrap(), Trigger::Schedule);
    }

    #[test]
    fn parse_trigger_gate() {
        assert_eq!(parse_trigger("gate").unwrap(), Trigger::Gate);
    }

    #[test]
    fn parse_trigger_watch_with_path() {
        assert_eq!(
            parse_trigger("watch:.ccteam/issues/").unwrap(),
            Trigger::Watch(PathBuf::from(".ccteam/issues/"))
        );
    }

    #[test]
    fn parse_trigger_watch_empty_is_parsed_then_caught_by_validate() {
        // parse succeeds (empty PathBuf)…
        let parsed = parse_trigger("watch:").unwrap();
        assert_eq!(parsed, Trigger::Watch(PathBuf::new()));
        // …but validate() rejects it.
        let spec = WorkflowSpec {
            name: "x".into(),
            description: None,
            mode: WorkflowMode::ArtifactDriven,
            enabled: true,
            budget: None,
            budgets_v060: None,
            agent_team: None,
            agents: {
                let mut m = IndexMap::new();
                m.insert(
                    "explorer".into(),
                    AgentSpec {
                        executor: Executor::Claude,
                        trigger: parsed,
                        parallelism: None,
                        input: None,
                        output: None,
                        interval: None,
                        timeout: None,
                        on_timeout: None,
                    },
                );
                m
            },
        };
        let err = spec.validate().unwrap_err();
        assert!(matches!(err, WorkflowError::ValidationFailed(_)));
    }

    #[test]
    fn parse_trigger_unknown_form_errors() {
        assert!(parse_trigger("foo").is_err());
        assert!(parse_trigger("cron:5m").is_err());
    }

    #[test]
    fn validate_role_name_accepts_kebab_snake_digits() {
        assert!(validate_role_name("explorer").is_ok());
        assert!(validate_role_name("fix-bot").is_ok());
        assert!(validate_role_name("agent_2").is_ok());
        assert!(validate_role_name("a").is_ok());
    }

    #[test]
    fn validate_role_name_rejects_upper_space_punct() {
        assert!(validate_role_name("Explorer").is_err());
        assert!(validate_role_name("my agent").is_err());
        assert!(validate_role_name("agent.x").is_err());
        assert!(validate_role_name("").is_err());
    }
}
