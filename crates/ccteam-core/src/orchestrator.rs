//! ccteam orchestrator main loop. M0 wires the bare-bones daemon:
//!
//! - load + M0-validate every phase template under
//!   `~/.ccteam/<team.phase_dir>/` on startup, per-team
//!   (fail-fast on `parallelism != solo` per development-plan
//!   §2.1 M0.6 acceptance);
//! - 30s tick (configurable for tests);
//! - notify-rs watcher on `~/.ccteam/progress/` so the loop wakes when
//!   a hook appends an event;
//! - cancellable run via a caller-supplied shutdown future.
//!
//! M3.3: the loader is team-aware. `Orchestrator::new` scans
//! `~/.ccteam/teams/<name>/team.yaml` and registers per-team
//! `TeamRuntime { spec, templates, dag }`. Legacy installs that
//! never ran `ccteam init --force` and only have `~/.ccteam/phases/`
//! still load — that path is registered as an implicit dev team
//! with default `TeamSpec`.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::{json, Value};

use crate::cost::{self, CostLevel};
use crate::dag::Dag;
use crate::auto_loop::{self, AutoLoopState};
use crate::inbox::{InboxMessage, SessionMailbox};
// `META_TEAM_NAME` is no longer referenced from orchestrator after
// V0.2 §6.4 candidate 5 — evergreen-team behavior dispatches off
// `TeamSpec::evergreen` / `cost_policy` flags instead.
use crate::paths::CcteamPaths;
use crate::phases::{PhaseTemplate, SubSkillTrigger};
use crate::progress;
use crate::stall::{self, StallLevel, StallThresholds};
use crate::state::{PhaseHistoryEntry, PhaseState, ProjectState};
use crate::subskill::{self, ClaudePRunner, SubSkillRunner};
use crate::team::TeamSpec;
use crate::tmux::TmuxSession;
use crate::tool_surface::{user_claude_dir, ToolSurfaceSnapshot};

/// Per-team runtime: parsed `team.yaml` + the phase templates loaded
/// from `<root>/<team.phase_dir>/` + the DAG those templates infer.
/// `Orchestrator::process_project` looks this up by `state.team` and
/// uses the right `dag` for state-machine transitions, so dev and
/// product-research can run concurrently without a global team flag.
#[derive(Debug, Clone)]
pub struct TeamRuntime {
    pub spec: TeamSpec,
    pub templates: Vec<PhaseTemplate>,
    pub dag: Dag,
}

/// M1.2: how many regular project sessions can run concurrently. Hard-
/// coded for M1; M3 may move it to `team.yaml` / global config. The
/// meta-agent session is **not counted** — it's a permanent fixture in
/// the User Interaction Layer.
pub const MAX_CONCURRENT_PROJECTS: usize = 3;

/// Pure decision function: given the current `state` and the last
/// progress.jsonl event, what should the orchestrator do next?
/// Side-effecting follow-up lives in `Orchestrator::process_project`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TickAction {
    /// Nothing to do (still in-flight, or terminal state reached).
    NoOp,
    /// claude completed `from`; advance state to `to` (None => DAG end).
    AdvancePhase {
        from: String,
        to: Option<String>,
    },
    /// **M3.6**: claude printed `ESCALATE: PHASE_DONE_PENDING — ...`
    /// (interfaces §4.1.1). The phase produced its required outputs but
    /// flagged some sub-tasks as deferred. The orchestrator will advance
    /// to `to` only when its `required_inputs` does not overlap
    /// `open_decisions`; otherwise the project parks in `DonePending`
    /// state until the user resumes.
    AdvancePhasePending {
        from: String,
        to: Option<String>,
        open_decisions: Vec<String>,
    },
    /// claude printed `ESCALATE: <reason>`. Mark the project escalated.
    Escalated {
        phase: String,
        reason: String,
    },
    /// claude is idle; orchestrator should inject the named phase prompt.
    DispatchPhase {
        phase: String,
    },
}

pub fn decide_tick(dag: &Dag, state: &ProjectState, last_event: Option<&Value>) -> TickAction {
    decide_tick_from_events(dag, state, last_event.map(std::slice::from_ref).unwrap_or(&[]))
}

/// Like `decide_tick` but considers recent events as a slice rather
/// than just the last one. The slice variant is needed because Claude
/// Code fires `SubagentStop` after `Stop`, and parse-phase-end's
/// `phase_done` lands between the two — the literal last event in the
/// JSONL is `SubagentStop`, but the project did finish.
///
/// `events` should be ordered oldest→newest. `decide_tick` continues
/// to forward a single event for backwards-compat with old call sites.
pub fn decide_tick_from_events(
    dag: &Dag,
    state: &ProjectState,
    events: &[Value],
) -> TickAction {
    if dag.is_empty() {
        // No phases loaded — orchestrator is inert until templates land.
        return TickAction::NoOp;
    }
    if dag.is_terminal_state(state) {
        return TickAction::NoOp;
    }

    // DonePending parks the project until the user resumes; no event
    // can drive it forward automatically (open decisions are by
    // definition outside the autonomous loop). M3.6 keeps this minimal
    // — the resume CLI clears the state, which lets the next tick
    // re-evaluate the advance check.
    if matches!(state.phase_state, PhaseState::DonePending { .. }) {
        return TickAction::NoOp;
    }

    if matches!(
        state.phase_state,
        PhaseState::InFlight | PhaseState::AutoLocked
    ) {
        if events.is_empty() {
            return TickAction::NoOp; // dispatched but no events yet
        }
        if let Some(terminal) =
            crate::progress::latest_terminal_event_for_phase(events, &state.current_phase)
        {
            let kind = terminal
                .get("event")
                .and_then(|s| s.as_str())
                .unwrap_or("");
            return match kind {
                "phase_done" => TickAction::AdvancePhase {
                    from: state.current_phase.clone(),
                    to: dag.next_on_done(&state.current_phase).map(String::from),
                },
                "phase_done_pending" => {
                    let open_decisions: Vec<String> = terminal
                        .get("open_decisions")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|s| s.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default();
                    TickAction::AdvancePhasePending {
                        from: state.current_phase.clone(),
                        to: dag.next_on_done(&state.current_phase).map(String::from),
                        open_decisions,
                    }
                }
                "escalate" => {
                    let reason = terminal
                        .get("reason")
                        .and_then(|s| s.as_str())
                        .unwrap_or("(no reason given)")
                        .to_string();
                    TickAction::Escalated {
                        phase: state.current_phase.clone(),
                        reason,
                    }
                }
                _ => TickAction::NoOp,
            };
        }
        TickAction::NoOp
    } else {
        // Idle: dispatch the current phase, defaulting to the DAG's
        // entry node (e.g. dev's `plan-eng`, research's `00-topic`).
        let phase = if state.current_phase.is_empty() {
            dag.entry().to_string()
        } else {
            state.current_phase.clone()
        };
        TickAction::DispatchPhase { phase }
    }
}

#[derive(Debug, Clone)]
pub struct OrchestratorConfig {
    /// How often the main loop ticks in the absence of progress events.
    pub tick_interval: Duration,
    /// argv for the in-pane process when (re)starting a project's tmux
    /// session. Default is `claude --dangerously-skip-permissions`;
    /// tests substitute a sleeping shell so `--dangerously-skip-…`
    /// doesn't hit the real CLI.
    pub claude_argv: Vec<String>,
    /// How long the context-reset routine waits for SessionStart's
    /// ready marker before bailing.
    pub ready_timeout: Duration,
    /// Extra delay between SessionStart's ready marker landing and the
    /// first send-keys. The marker fires during claude's CLI bootstrap,
    /// before the TUI input is actually focused — without this delay the
    /// first prompt sits in the input box but the Enter never registers.
    /// Tests with shell stand-ins set this to ~0; production keeps a
    /// short couple-of-seconds buffer.
    pub post_ready_warmup: Duration,
    /// Skip the M0.5.3 startup `tools_required` check. Useful when an
    /// older project on disk references plugin agents that aren't yet
    /// linked in, and the user wants the orchestrator to come up
    /// anyway so they can run `ccteam doctor --install-recommended-agents`.
    pub skip_tool_check: bool,
    /// argv for the M2.1 sub-skill runner. Default mirrors the
    /// production `claude -p ...` shape from `ClaudePRunner::default`;
    /// tests can override with a shell stub that echoes stdin to
    /// stdout, so the orchestrator's sub-skill plumbing is exercised
    /// without spawning a real claude.
    pub subskill_argv: Option<Vec<String>>,
}

/// Production model identifier passed to `claude --model`. The `[1m]`
/// suffix is Claude Code's documented opt-in to the 1M-token context
/// window. tech-design §6.1 / §6.9 require the long context for cache
/// reuse + the 60% phase-boundary reset budget.
///
/// V0.2 §7 / dev-plan §9 M0.23.2: 1M default. When Anthropic publishes
/// a newer Sonnet alias, change this single line.
pub const DEFAULT_CLAUDE_MODEL: &str = "claude-sonnet-4-6[1m]";

impl Default for OrchestratorConfig {
    fn default() -> Self {
        // F29 — `CCTEAM_CLAUDE_ARGV` lets CLI / e2e harness inject a
        // stub claude (eg `sh -c 'echo …'`) without rebuilding the
        // binary. Whitespace-split for shell-style invocation; empty /
        // unset = production default below. CLI flag still wins via
        // an explicit `OrchestratorConfig.claude_argv` assignment in
        // `ccteam start`.
        let claude_argv = std::env::var("CCTEAM_CLAUDE_ARGV")
            .ok()
            .and_then(|raw| {
                let parts: Vec<String> = raw
                    .split_whitespace()
                    .map(String::from)
                    .collect();
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
            subskill_argv: None,
        }
    }
}

#[derive(Debug)]
pub struct Orchestrator {
    paths: CcteamPaths,
    config: OrchestratorConfig,
    /// Per-team runtimes keyed by team name. Populated at startup by
    /// `load_team_runtimes`.
    teams: HashMap<String, TeamRuntime>,
    /// Empty fallback returned by `dag()` / `templates()` when no
    /// teams loaded — preserves the pre-M3.3 `&Dag` / `&[PhaseTemplate]`
    /// API for tests that asserted "orchestrator with empty phases dir
    /// is inert".
    empty: TeamRuntime,
}

impl Orchestrator {
    /// Construct + validate. Returns `Err` if any phase template
    /// fails M0 validation in any registered team, so the daemon
    /// refuses to start in a known-bad state instead of silently
    /// routing through agent_team / multi_session paths that aren't
    /// implemented yet.
    ///
    /// **Pure load** — no side effects on `~/.ccteam/`. Production
    /// callers seed shipped templates via `ccteam start` /
    /// `ccteam init` / `ccteam doctor --reset-shipped-teams` before
    /// constructing the orchestrator (V0.2 M0.16.2 — keeps tests
    /// from picking up stray shipped teams when they construct an
    /// Orchestrator against an empty tempdir).
    pub fn new(paths: CcteamPaths, config: OrchestratorConfig) -> Result<Self> {
        let teams = load_team_runtimes(&paths)?;
        if !config.skip_tool_check {
            for runtime in teams.values() {
                check_phase_tools(&runtime.templates).with_context(|| {
                    format!("team `{}` tool surface check", runtime.spec.name)
                })?;
            }
        } else {
            tracing::warn!("skip_tool_check=true: phase tools_required not validated");
        }
        for runtime in teams.values() {
            tracing::info!(
                team = %runtime.spec.name,
                templates = runtime.templates.len(),
                phase_dir = %runtime.spec.phase_dir,
                entry_phase = %runtime.dag.entry(),
                "team runtime registered",
            );
        }
        let empty = TeamRuntime {
            spec: TeamSpec {
                name: String::new(),
                aliases: Vec::new(),
                description: String::new(),
                retro_schema: Vec::new(),
                critic_dimensions: Vec::new(),
                escalate_grammar_extensions: Vec::new(),
                golden_rules: crate::team::TeamGoldenRules::default(),
                phase_dir: "phases".into(),
                verdict_schema: Vec::new(),
                evergreen: false,
                cost_policy: crate::team::CostPolicy::default(),
                claude_md_template: String::new(),
            },
            templates: Vec::new(),
            dag: Dag::from_templates(&[])?,
        };
        Ok(Self {
            paths,
            config,
            teams,
            empty,
        })
    }

    /// Returns dev's templates (or empty when dev isn't loaded). Kept
    /// for backwards compat with M0–M2 tests; new code should use
    /// `team_runtime(name)` for per-team access.
    pub fn templates(&self) -> &[PhaseTemplate] {
        self.team_runtime("dev")
            .map(|t| t.templates.as_slice())
            .unwrap_or(&self.empty.templates)
    }

    /// Returns dev's DAG (or empty when dev isn't loaded). Kept
    /// for backwards compat with M0–M2 tests; new code should use
    /// `team_runtime(name)` for per-team access.
    pub fn dag(&self) -> &Dag {
        self.team_runtime("dev")
            .map(|t| &t.dag)
            .unwrap_or(&self.empty.dag)
    }

    /// Look up the runtime for `team`. Returns `None` for unknown
    /// teams (project's state.json carries an unknown team string).
    ///
    /// V0.2.2 F40 — alias-aware: when no runtime is keyed under the
    /// canonical name, scan every registered runtime's `spec.aliases`
    /// and return the first match. Lets old projects whose
    /// `state.json::team` carries a renamed team (e.g.
    /// `product-research` → `research`) still find the loaded runtime
    /// without forcing a data migration.
    pub fn team_runtime(&self, team: &str) -> Option<&TeamRuntime> {
        if let Some(rt) = self.teams.get(team) {
            return Some(rt);
        }
        self.teams
            .values()
            .find(|rt| rt.spec.aliases.iter().any(|a| a == team))
    }

    /// V0.2.1 F28 — project-scoped resolution. When the project carries
    /// a `<project_dir>/.ccteam/team/team.yaml`, that override wins over
    /// the global cache. Otherwise returns a borrowed handle to the
    /// cached runtime so the common case stays zero-allocation.
    ///
    /// `Cow` lets the rare project-override case return an owned
    /// `TeamRuntime` (rebuilt by re-resolving + reloading phase
    /// templates from the override's team_dir) while the dominant
    /// "no override" path keeps the original `&TeamRuntime` semantics.
    /// Callers that don't need the override can keep using
    /// [`Self::team_runtime`].
    pub fn team_runtime_for_state<'a>(
        &'a self,
        state: &ProjectState,
    ) -> Option<std::borrow::Cow<'a, TeamRuntime>> {
        let project_dir = self.paths.project_dir(&state.slug);
        let override_yaml = project_dir
            .join(".ccteam")
            .join("team")
            .join("team.yaml");
        if !override_yaml.exists() {
            return self
                .team_runtime(&state.team)
                .map(std::borrow::Cow::Borrowed);
        }
        // Project has an override — re-resolve + rebuild a TeamRuntime
        // from the override's team_dir. The override is per-project so
        // we deliberately do NOT cache.
        match build_project_team_runtime(&self.paths, state, &project_dir) {
            Ok(rt) => Some(std::borrow::Cow::Owned(rt)),
            Err(err) => {
                tracing::warn!(
                    team = %state.team,
                    slug = %state.slug,
                    error = format!("{err:#}"),
                    "project-layer team override failed to build; \
                     falling back to global cached runtime",
                );
                self.team_runtime(&state.team)
                    .map(std::borrow::Cow::Borrowed)
            }
        }
    }

    /// Iterate every registered team. Used by `ccteam doctor` and tests.
    pub fn teams(&self) -> impl Iterator<Item = &TeamRuntime> {
        self.teams.values()
    }

    /// V0.2 §6.4 candidate 5: declarative replacement for
    /// `state.team == META_TEAM_NAME` branches. Evergreen sessions
    /// bypass phase-DAG advance, cost hard-kill, and stall warnings.
    /// An unknown team (no runtime registered) defaults to `false` —
    /// matching the historical behavior where only meta-agent fell
    /// into the special branch and everything else ran the regular
    /// phase-DAG path.
    pub(crate) fn is_evergreen(&self, team: &str) -> bool {
        self.team_runtime(team)
            .is_some_and(|t| t.spec.evergreen)
    }

    /// V0.2 §6.4 candidate 5: cost policy lookup. Defaults to
    /// `KillAt(None)` (the historical behavior — hard-kill at
    /// `state.hard_kill_threshold_usd`) for any unknown team so
    /// dropping a team.yaml without `cost_policy:` keeps M3 semantics.
    pub(crate) fn cost_policy(&self, team: &str) -> crate::team::CostPolicy {
        self.team_runtime(team)
            .map(|t| t.spec.cost_policy)
            .unwrap_or_default()
    }

    pub fn paths(&self) -> &CcteamPaths {
        &self.paths
    }

    /// Inject `phase`'s prompt into `slug`'s tmux session, picking
    /// `send-keys` vs `/btw` based on the last progress event
    /// (tech-design §6.9 idle-aware injection). Appends a
    /// `phase_inject` event so the next idle-check sees a non-idle
    /// state until claude either runs a tool (PreToolUse arrives) or
    /// the prompt finishes and a Stop fires.
    pub fn dispatch_phase(&self, slug: &str, phase: &str) -> Result<()> {
        let state_path = self.paths.project_state(slug);
        let state = ProjectState::load(&state_path)
            .with_context(|| format!("load state for dispatch {slug}"))?;
        self.dispatch_phase_with_state(slug, phase, &state)
    }

    /// Same as `dispatch_phase` but the caller passes the already-loaded
    /// `ProjectState`. Used inside `process_project` to avoid a redundant
    /// reload (and so meta-agent paths can pass their bespoke state).
    pub fn dispatch_phase_with_state(
        &self,
        slug: &str,
        phase: &str,
        state: &ProjectState,
    ) -> Result<()> {
        let progress_path = self.paths.progress_jsonl(slug);
        let last = progress::last_event(&progress_path)?;
        let idle = progress::is_idle(last.as_ref());

        let attachments = self.attachments_for_next_phase(slug, state);
        let attachment_refs: Vec<&str> = attachments.iter().map(String::as_str).collect();
        // V0.2 M0.18: prefer the template-aware inject builder when the
        // team / phase is registered. Falls back to the legacy name-only
        // shape only when the team or template is missing (e.g. an
        // unknown team `state.team` slipped past validation), keeping
        // the dispatch path resilient.
        // V0.2.1 F28: project-layer override wins via
        // `team_runtime_for_state`.
        let prompt = match self.team_runtime_for_state(state) {
            Some(team) => match team.templates.iter().find(|tpl| tpl.name == phase) {
                Some(template) => {
                    let protocol_dirs: Vec<&str> = team
                        .spec
                        .golden_rules
                        .protocol
                        .iter()
                        .filter(|r| {
                            r.enforce
                                == crate::team::GoldenRuleEnforcement::PromptDirective
                        })
                        .filter_map(|r| r.directive.as_deref())
                        .collect();
                    progress::build_phase_prompt_for_template_with_team(
                        template,
                        &attachment_refs,
                        &protocol_dirs,
                    )
                }
                None => progress::build_phase_prompt_with_attachments(phase, &attachment_refs),
            },
            None => progress::build_phase_prompt_with_attachments(phase, &attachment_refs),
        };
        let message = progress::idle_aware_message(&prompt, idle);

        let session = TmuxSession::from_name(state.tmux_session.clone());
        session
            .send_keys(&message)
            .with_context(|| format!("send phase prompt to {}", session.name()))?;

        let event = json!({
            "ts": Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            "event": "phase_inject",
            "phase": phase,
            "idle": idle,
            "attachments": attachments,
        });
        progress::append_event(&progress_path, &event)?;
        Ok(())
    }

    /// Collect `output_to` paths from the previous phase's `phase_done`
    /// sub-skills that exist on disk. The orchestrator passes these
    /// to `build_phase_prompt_with_attachments` so the next phase's
    /// claude reads them via `@<path>` without the phase markdown
    /// having to know which sub-skills produced them.
    ///
    /// Pure heuristic: previous phase = `phase_history.last()`. If
    /// the project's team isn't registered, the phase template doesn't
    /// exist, or no sub-skills wrote anything, returns an empty vec.
    /// Public for direct testing (the dispatch path that consumes this
    /// is hard to drive in unit tests because of the tmux dependency).
    pub fn attachments_for_next_phase(
        &self,
        slug: &str,
        state: &ProjectState,
    ) -> Vec<String> {
        let Some(prev) = state.phase_history.last() else {
            return Vec::new();
        };
        // V0.2.1 F28: project-layer override wins.
        let Some(team) = self.team_runtime_for_state(state) else {
            return Vec::new();
        };
        let Some(template) = team.templates.iter().find(|t| t.name == prev.phase) else {
            return Vec::new();
        };
        let project_dir = self.paths.project_dir(slug);
        template
            .sub_skills
            .iter()
            .filter(|s| s.trigger == SubSkillTrigger::PhaseDone)
            .filter_map(|s| {
                let abs = project_dir.join(&s.output_to);
                abs.exists().then(|| s.output_to.clone())
            })
            .collect()
    }

    /// Construct the configured sub-skill runner. Honors
    /// `OrchestratorConfig::subskill_argv` so tests can swap a stub.
    fn build_subskill_runner(&self) -> Box<dyn SubSkillRunner> {
        match &self.config.subskill_argv {
            Some(argv) => Box::new(ClaudePRunner::with_argv(argv.clone())),
            None => Box::<ClaudePRunner>::default(),
        }
    }

    /// Run every `phase_start` / `phase_done` sub-skill in `template`.
    /// The orchestrator calls this directly around `AdvancePhase` and
    /// `DispatchPhase` transitions in `process_project`.
    pub fn run_phase_sub_skills(
        &self,
        slug: &str,
        template: &PhaseTemplate,
        trigger: SubSkillTrigger,
    ) {
        if template.sub_skills.is_empty() {
            return;
        }
        let project_dir = self.paths.project_dir(slug);
        let progress_path = self.paths.progress_jsonl(slug);
        let runner = self.build_subskill_runner();
        let _outputs = subskill::run_sub_skills_for_phase(
            template,
            trigger,
            &project_dir,
            &progress_path,
            runner.as_ref(),
        );
    }

    /// Scan `~/projects/*/.ccteam/state.json` and return one entry per
    /// loaded project. Directories without a state.json are skipped
    /// silently; broken state.json files surface as errors.
    pub fn discover_projects(&self) -> Result<Vec<(String, ProjectState)>> {
        let dir = &self.paths.projects_root;
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir)
            .with_context(|| format!("read_dir {}", dir.display()))?
        {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let Some(slug) = name.to_str() else { continue };
            let state_path = self.paths.project_state(slug);
            if !state_path.exists() {
                continue;
            }
            match ProjectState::load(&state_path) {
                Ok(state) => out.push((slug.to_string(), state)),
                Err(err) => {
                    tracing::warn!(
                        slug,
                        path = %state_path.display(),
                        error = %err,
                        "skip project: state.json failed to load",
                    );
                }
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }

    /// Reset claude's context at a phase boundary (tech-design §6.9).
    ///
    /// Appends a progress summary to the project's CLAUDE.md, then
    /// recycles the tmux session so the new claude reads the bridged
    /// summary instead of the bloated history. M0 kills + restarts
    /// the tmux session under the same name (same end-state as
    /// `/exit` + re-launch in the same window, simpler to express).
    pub fn reset_context(&self, slug: &str, state: &mut ProjectState) -> Result<()> {
        let project_dir = self.paths.project_dir(slug);
        let claude_md = project_dir.join("CLAUDE.md");
        let summary = build_progress_summary(state);
        append_progress_summary(&claude_md, &summary)
            .with_context(|| format!("bridge progress to {}", claude_md.display()))?;

        let session = TmuxSession::from_name(state.tmux_session.clone());
        let ready = CcteamPaths::project_ready_in(&project_dir);
        if ready.exists() {
            let _ = std::fs::remove_file(&ready);
        }
        session
            .kill()
            .with_context(|| format!("kill tmux session for {slug}"))?;

        let argv: Vec<&str> = self.config.claude_argv.iter().map(|s| s.as_str()).collect();
        session
            .start(&project_dir, &argv)
            .with_context(|| format!("restart tmux session for {slug}"))?;

        wait_for_ready(&ready, self.config.ready_timeout)
            .with_context(|| format!("wait for SessionStart ready marker for {slug}"))?;

        state.context_tokens_used = 0;
        state.context_reset_count = state.context_reset_count.saturating_add(1);
        state.last_event_type = Some("context_reset".into());
        state.last_progress_event_at = Some(Utc::now());
        state.save(&self.paths.project_state(slug))?;

        progress::append_event(
            &self.paths.progress_jsonl(slug),
            &json!({
                "ts": Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
                "event": "context_reset",
                "reset_count": state.context_reset_count,
            }),
        )?;
        tracing::info!(
            slug,
            reset_count = state.context_reset_count,
            "context reset complete",
        );
        Ok(())
    }

    /// Idempotently ensure `slug`'s tmux session is up before we try to
    /// `send-keys` into it. Three states:
    ///
    /// - alive (`is_alive(claude_pid)` true) → no-op;
    /// - present-but-pid-mismatched / dead pane → kill + recreate so
    ///   we don't reattach to a zombie that won't accept keys;
    /// - missing → fresh `start` + wait for SessionStart's ready marker.
    ///
    /// Mutates + persists `state.claude_pid` whenever a new session is
    /// born. Skipped silently for terminal projects.
    pub fn ensure_session(&self, slug: &str, state: &mut ProjectState) -> Result<()> {
        // Evergreen sessions never reach a terminal phase state and the
        // dag check below would skip them — guard evergreen first so it
        // always tries to come up. (§6.4 candidate 5 declarative replacement
        // for the prior `state.team == META_TEAM_NAME` literal.)
        let is_evergreen = self.is_evergreen(&state.team);
        // V0.2.1 F28: project-layer override wins for the DAG used to
        // gate the "terminal state" early-exit.
        let team_for_dag = self.team_runtime_for_state(state);
        let team_dag = team_for_dag.as_deref().map(|t| &t.dag);
        if !is_evergreen && team_dag.is_some_and(|d| d.is_terminal_state(state)) {
            return Ok(());
        }
        let session = TmuxSession::from_name(state.tmux_session.clone());
        if session.is_alive(state.claude_pid) {
            return Ok(());
        }
        // Either the session is missing or the cached PID is stale —
        // tear down whatever might be left and start clean.
        if session.exists() {
            session
                .kill()
                .with_context(|| format!("kill stale tmux session for {slug}"))?;
        }
        let project_dir = self.paths.project_dir(slug);
        let ready = CcteamPaths::project_ready_in(&project_dir);
        if ready.exists() {
            // Old marker from a previous session — remove so
            // wait_for_ready measures the *new* SessionStart.
            let _ = std::fs::remove_file(&ready);
        }
        let argv: Vec<&str> = self.config.claude_argv.iter().map(|s| s.as_str()).collect();
        session
            .start(&project_dir, &argv)
            .with_context(|| format!("start tmux session for {slug}"))?;
        wait_for_ready(&ready, self.config.ready_timeout)
            .with_context(|| format!("wait for SessionStart ready marker for {slug}"))?;
        // SessionStart's ready marker fires during claude's CLI bootstrap,
        // *before* the TUI input field is actually focused. Without a
        // small warmup, the first phase prompt lands in the input box but
        // the Enter keypress is silently dropped — the project then sits
        // forever with the prompt visible but never executed.
        if !self.config.post_ready_warmup.is_zero() {
            std::thread::sleep(self.config.post_ready_warmup);
        }
        state.claude_pid = session.pane_pid()?;
        state.last_progress_event_at = Some(Utc::now());
        state.last_event_type = Some("session_start".into());
        state.save(&self.paths.project_state(slug))?;
        tracing::info!(
            slug,
            tmux_session = %session.name(),
            claude_pid = ?state.claude_pid,
            "tmux session started",
        );
        Ok(())
    }

    /// Apply `decide_tick` to one project, looping the AdvancePhase →
    /// DispatchPhase chain so a phase boundary doesn't take two ticks
    /// to act on. The loop iteration cap is a paranoia guard.
    ///
    /// Evergreen projects (`team.yaml.evergreen: true`) bypass the
    /// phase DAG entirely — they're event-loop sessions, not phase-DAG
    /// ones — and only get `ensure_session` lifecycle handling. See
    /// `process_meta_project` for that path. (§6.4 candidate 5.)
    pub fn process_project(&self, slug: &str, mut state: ProjectState) -> Result<ProjectState> {
        if self.is_evergreen(&state.team) {
            return self.process_meta_project(slug, state);
        }
        // Resolve the project's team runtime. Unknown team in
        // state.json is a misconfiguration — we log + skip rather
        // than panicking so a single broken project doesn't take the
        // whole orchestrator down.
        // V0.2.1 F28: project-layer override wins for the duration of
        // this `process_project` call (re-resolved per tick, no cache).
        let team_cow = match self.team_runtime_for_state(&state) {
            Some(t) => t,
            None => {
                tracing::error!(
                    slug,
                    team = %state.team,
                    "no team runtime registered for project; \
                     run `ccteam init` to populate ~/.ccteam/teams/<team>/team.yaml",
                );
                return Ok(state);
            }
        };
        let team: &TeamRuntime = team_cow.as_ref();
        const MAX_ITERS: u32 = 4;
        let progress_path = self.paths.progress_jsonl(slug);
        let state_path = self.paths.project_state(slug);

        for _ in 0..MAX_ITERS {
            let events = progress::read_all_events(&progress_path)?;
            let action = decide_tick_from_events(&team.dag, &state, &events);
            match action {
                TickAction::NoOp => return Ok(state),
                TickAction::AdvancePhase { from, to } => {
                    // M2.1: phase_done sub-skills run *before* state
                    // advance so their output files are on disk by the
                    // time the next phase prompt is built (the prompt
                    // builder pulls in @-attachments from the prior
                    // phase's sub_skills outputs).
                    if let Some(prev_template) =
                        team.templates.iter().find(|t| t.name == from)
                    {
                        self.run_phase_sub_skills(
                            &slug.to_string(),
                            prev_template,
                            SubSkillTrigger::PhaseDone,
                        );

                        // M2.3 follow-up: golden_rules enforcement.
                        // Sub-skills have produced their artifacts;
                        // now check the phase's hard contract before
                        // declaring it passed. Decision §4.3 (c) —
                        // orchestrator-owned post-PHASE_DONE check, not
                        // a phase `after` hook (which would be too
                        // easy for the phase prompt to bypass) and not
                        // the Stop hook (which shouldn't do heavy
                        // work). Violations block the advance and
                        // route through the same escalation flow as
                        // ESCALATE: the user fixes, then `ccteam
                        // resume <slug>` re-arms the phase for retry.
                        if !prev_template.golden_rules.is_empty() {
                            let project_dir = self.paths.project_dir(slug);
                            match crate::golden_rules::enforce(
                                prev_template,
                                &project_dir,
                            ) {
                                Ok(report) if !report.is_pass() => {
                                    if let Err(err) = self
                                        .handle_golden_rules_violation(
                                            slug,
                                            &from,
                                            &report,
                                            &mut state,
                                            &state_path,
                                        )
                                    {
                                        tracing::error!(
                                            slug,
                                            phase = %from,
                                            error = %err,
                                            "golden_rules violation handler failed",
                                        );
                                    }
                                    return Ok(state);
                                }
                                Ok(report) => {
                                    // PASS — log skipped rules so phase
                                    // author sees malformed regex etc.
                                    if !report.skipped.is_empty() {
                                        for s in &report.skipped {
                                            tracing::warn!(
                                                slug,
                                                phase = %from,
                                                rule = %s.rule_id,
                                                reason = %s.reason,
                                                "golden_rule skipped",
                                            );
                                        }
                                    }
                                    let event = serde_json::json!({
                                        "event": "golden_rules_check",
                                        "phase": from,
                                        "result": "pass",
                                        "passed": report.passed,
                                        "skipped": report.skipped,
                                        "ts": Utc::now().to_rfc3339(),
                                    });
                                    let _ = progress::append_event(
                                        &progress_path,
                                        &event,
                                    );
                                }
                                Err(err) => {
                                    // Couldn't even run enforcement —
                                    // not a phase fault, log and
                                    // continue. We don't want infra
                                    // hiccups to block dev work.
                                    tracing::warn!(
                                        slug,
                                        phase = %from,
                                        error = %err,
                                        "golden_rules enforce failed; continuing",
                                    );
                                }
                            }
                        }
                    }
                    state.phase_history.push(PhaseHistoryEntry {
                        phase: from.clone(),
                        status: "passed".into(),
                        duration_s: 0,
                        cost_usd: 0.0,
                    });
                    state.phase_state = PhaseState::Idle;
                    state.current_phase = to.unwrap_or_default();
                    state.last_progress_event_at = Some(Utc::now());
                    state.last_event_type = Some("phase_done".into());
                    state.save(&state_path)?;
                    tracing::info!(
                        slug,
                        from = %from,
                        to = %state.current_phase,
                        "phase advanced",
                    );

                    // Phase boundary: maybe reset context if the
                    // 60%-of-1M threshold has been crossed.
                    if !team.dag.is_terminal_state(&state)
                        && state.context_tokens_used > state.context_reset_threshold_tokens
                    {
                        if let Err(err) = self.reset_context(slug, &mut state) {
                            tracing::error!(
                                slug,
                                error = %err,
                                "context reset failed; continuing without reset",
                            );
                        }
                    }
                    // continue the loop so a fresh idle state can
                    // dispatch the next phase in the same tick.
                }
                TickAction::AdvancePhasePending {
                    from,
                    to,
                    open_decisions,
                } => {
                    // M3.6: phase finished + flagged some decisions as
                    // deferred. Compute the static intersection between
                    // open_decisions and the next phase's required_inputs.
                    let next_template = to
                        .as_ref()
                        .and_then(|name| team.templates.iter().find(|t| &t.name == name));
                    let blocking: Vec<String> = match next_template {
                        Some(t) => intersect_open_decisions_with_required_inputs(
                            &open_decisions,
                            &t.required_inputs,
                        ),
                        None => Vec::new(),
                    };

                    if let Some(prev_template) =
                        team.templates.iter().find(|t| t.name == from)
                    {
                        self.run_phase_sub_skills(
                            &slug.to_string(),
                            prev_template,
                            SubSkillTrigger::PhaseDone,
                        );
                    }

                    state.phase_history.push(PhaseHistoryEntry {
                        phase: from.clone(),
                        status: "passed".into(),
                        duration_s: 0,
                        cost_usd: 0.0,
                    });
                    state.last_progress_event_at = Some(Utc::now());
                    state.last_event_type = Some("phase_done_pending".into());

                    if blocking.is_empty() {
                        // Safe to advance — no decision-dependent
                        // required_inputs in the next phase. Project
                        // continues; user can answer outbox files
                        // out-of-band without blocking the pipeline.
                        state.phase_state = PhaseState::Idle;
                        state.current_phase = to.unwrap_or_default();
                        state.save(&state_path)?;
                        tracing::info!(
                            slug,
                            from = %from,
                            to = %state.current_phase,
                            open_decisions = ?open_decisions,
                            "phase advanced past PHASE_DONE_PENDING; \
                             open decisions do not block next phase",
                        );
                        // continue loop to dispatch next phase.
                    } else {
                        // Block: park in DonePending, write escalation,
                        // wait for `ccteam resume`.
                        state.phase_state = PhaseState::DonePending {
                            open_decisions: open_decisions.clone(),
                        };
                        // current_phase stays at `from` so peek/show
                        // surface the *deferred* phase, not a phase the
                        // user hasn't seen yet.
                        let esc_path =
                            self.paths.project_ccteam_dir(slug).join("escalation.md");
                        if let Some(parent) = esc_path.parent() {
                            std::fs::create_dir_all(parent)
                                .with_context(|| format!("create {}", parent.display()))?;
                        }
                        let blocking_list = blocking.join(", ");
                        let next_name = to.as_deref().unwrap_or("(DAG endpoint)");
                        let body = format!(
                            "# Escalation (PHASE_DONE_PENDING)\n\n\
                             phase: {from}\n\
                             next phase: {next_name}\n\
                             blocking decisions: {blocking_list}\n\
                             open decisions: {}\n\n\
                             phase {from} produced its outputs but flagged decisions in \
                             outbox files. The next phase ({next_name}) requires those \
                             files (they are listed in its `required_inputs`).\n\n\
                             To continue:\n\
                             1. Resolve the open decisions (answer the outbox files, \
                                write replies, or update inputs).\n\
                             2. Run `ccteam resume {slug}` to re-evaluate the advance check.\n",
                            open_decisions.join(", "),
                        );
                        std::fs::write(&esc_path, body)
                            .with_context(|| format!("write {}", esc_path.display()))?;
                        state.save(&state_path)?;
                        tracing::warn!(
                            slug,
                            from = %from,
                            blocking = ?blocking,
                            "PHASE_DONE_PENDING blocked: next phase depends on open decisions",
                        );
                        return Ok(state);
                    }
                }
                TickAction::Escalated { phase, reason } => {
                    state.phase_history.push(PhaseHistoryEntry {
                        phase: phase.clone(),
                        status: "escalated".into(),
                        duration_s: 0,
                        cost_usd: 0.0,
                    });
                    state.phase_state = PhaseState::Idle;
                    state.last_progress_event_at = Some(Utc::now());
                    state.last_event_type = Some("escalate".into());
                    let esc_path =
                        self.paths.project_ccteam_dir(slug).join("escalation.md");
                    if let Some(parent) = esc_path.parent() {
                        std::fs::create_dir_all(parent)
                            .with_context(|| format!("create {}", parent.display()))?;
                    }
                    let body = format!(
                        "# Escalation\n\nphase: {phase}\nreason: {reason}\n\nrun `ccteam resume <slug>` once the underlying issue is fixed.\n",
                    );
                    std::fs::write(&esc_path, body)
                        .with_context(|| format!("write {}", esc_path.display()))?;
                    state.save(&state_path)?;
                    tracing::warn!(slug, phase = %phase, reason = %reason, "project escalated");
                    return Ok(state);
                }
                TickAction::DispatchPhase { phase } => {
                    // M0.7: orchestrator owns the tmux session lifecycle.
                    // Without this call, a fresh project's first tick
                    // (and any tick after a session crash) would error
                    // out in `dispatch_phase → send_keys` because the
                    // session simply wouldn't exist.
                    self.ensure_session(slug, &mut state)?;
                    // M2.1: phase_start sub-skills run *before* the
                    // phase prompt is injected so their outputs are
                    // available when the phase begins. Failures land
                    // in progress.jsonl but never block the phase.
                    if let Some(template) = team.templates.iter().find(|t| t.name == phase) {
                        self.run_phase_sub_skills(
                            slug,
                            template,
                            SubSkillTrigger::PhaseStart,
                        );
                    }
                    self.dispatch_phase(slug, &phase)?;
                    let template = team.templates.iter().find(|t| t.name == phase);
                    let target_state = if template.is_some_and(|t| t.auto_loop) {
                        // Stop hook (M0.12) drives the loop; orchestrator
                        // only re-enters on phase_done/escalate.
                        let t = template.expect("auto_loop branch implies template present");
                        let project_dir = self.paths.project_dir(slug);
                        // V0.2 M0.18: re-prompt the assistant with the
                        // same template-driven inject prompt the dispatch
                        // path used, so auto-loop iterations see the
                        // same protocol directives instead of a stripped
                        // legacy shim.
                        let prompt = progress::build_phase_prompt_for_template(t, &[]);
                        let fl = AutoLoopState::new(
                            slug.to_string(),
                            prompt,
                            t.auto_loop_max_iterations,
                            t.effective_completion_signal(),
                        );
                        auto_loop::write(&auto_loop::path_in(&project_dir), &fl)?;
                        PhaseState::AutoLocked
                    } else {
                        PhaseState::InFlight
                    };
                    state.current_phase = phase;
                    state.phase_state = target_state;
                    state.save(&state_path)?;
                    return Ok(state);
                }
            }
        }
        tracing::warn!(slug, "process_project hit MAX_ITERS; possible state-machine bug");
        Ok(state)
    }

    /// Meta-agent dispatch path: ensure the long-lived tmux session is
    /// up and process any inbox messages. **Never** injects phase
    /// prompts — meta-agents drive themselves via NL inside their
    /// session, and the orchestrator only routes external messages
    /// (terminal attach / channel layer) into them.
    ///
    /// Block phase advance because at least one `golden_rules` rule
    /// in the just-finished phase reported a violation.
    ///
    /// Behaves like the `Escalated` arm of `process_project`: marks
    /// the phase entry `blocked`, leaves `phase_state` Idle so a
    /// `ccteam resume <slug>` re-arms it after the user fixes the
    /// underlying issue, writes a structured `escalation.md`, and
    /// records a `golden_rules_check` event with `result: fail` for
    /// the cross-project decisions queue (M1) to pick up.
    fn handle_golden_rules_violation(
        &self,
        slug: &str,
        from: &str,
        report: &crate::golden_rules::GoldenRulesReport,
        state: &mut ProjectState,
        state_path: &Path,
    ) -> Result<()> {
        state.phase_history.push(PhaseHistoryEntry {
            phase: from.to_string(),
            status: "blocked".into(),
            duration_s: 0,
            cost_usd: 0.0,
        });
        state.phase_state = PhaseState::Idle;
        state.last_progress_event_at = Some(Utc::now());
        state.last_event_type = Some("golden_rules_check".into());

        let esc_path = self.paths.project_ccteam_dir(slug).join("escalation.md");
        if let Some(parent) = esc_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let mut body = String::from("# Escalation: golden_rules violation\n\n");
        body.push_str(&format!("phase: {from}\n\n## Violations\n\n"));
        for v in &report.violations {
            body.push_str(&format!(
                "- **{}** ({:?}): {}\n",
                v.rule_id, v.kind, v.detail,
            ));
        }
        if !report.skipped.is_empty() {
            body.push_str("\n## Skipped (could not evaluate)\n\n");
            for s in &report.skipped {
                body.push_str(&format!("- **{}**: {}\n", s.rule_id, s.reason));
            }
        }
        body.push_str(
            "\nfix the underlying issue, then run `ccteam resume <slug>` to retry the phase.\n",
        );
        std::fs::write(&esc_path, body)
            .with_context(|| format!("write {}", esc_path.display()))?;

        let event = serde_json::json!({
            "event": "golden_rules_check",
            "phase": from,
            "result": "fail",
            "passed": report.passed,
            "violations": report.violations,
            "skipped": report.skipped,
            "ts": Utc::now().to_rfc3339(),
        });
        let progress_path = self.paths.progress_jsonl(slug);
        let _ = progress::append_event(&progress_path, &event);

        state.save(state_path)?;
        tracing::warn!(
            slug,
            phase = %from,
            violations = report.violations.len(),
            "phase blocked by golden_rules violation",
        );
        Ok(())
    }

    /// **M1.4 context-reset bridge**: meta-agents have no phase
    /// boundary, so the regular phase-edge reset (tech-design §6.9)
    /// can't trigger. Instead, we check the 60% threshold on every
    /// tick and recycle the session in place when crossed —
    /// `reset_context` appends a "current progress" snippet to the
    /// meta-agent's CLAUDE.md so the new claude resumes the conversation
    /// with continuity. M4.6 will replace this with the full
    /// claude-mem RAG flow; M1's bare bridge is enough to keep the
    /// session usable across the 1M ceiling.
    pub fn process_meta_project(
        &self,
        slug: &str,
        mut state: ProjectState,
    ) -> Result<ProjectState> {
        debug_assert!(
            self.is_evergreen(&state.team),
            "process_meta_project called on non-evergreen team `{}`",
            state.team,
        );
        self.ensure_session(slug, &mut state)?;
        if let Err(err) = self.process_session_inbox(slug, &state) {
            tracing::warn!(slug, error = %err, "meta inbox processing failed");
        }
        if state.context_tokens_used > state.context_reset_threshold_tokens {
            tracing::info!(
                slug,
                tokens = state.context_tokens_used,
                threshold = state.context_reset_threshold_tokens,
                "meta-agent context reset triggered (M1.4)",
            );
            if let Err(err) = self.reset_context(slug, &mut state) {
                tracing::error!(
                    slug,
                    error = %err,
                    "meta context reset failed; conversation continuity may be lost",
                );
            }
        }
        Ok(state)
    }

    /// Drain inbox/ for `slug` (M1.1). Each message is read, injected
    /// via tmux send-keys (idle-aware), and then deleted — exactly
    /// matching the §3.4.5 protocol. Errors per-file are logged; one
    /// bad message must not stall the others.
    pub fn process_session_inbox(
        &self,
        slug: &str,
        state: &ProjectState,
    ) -> Result<()> {
        let cc = self.paths.project_ccteam_dir(slug);
        let mailbox = SessionMailbox::for_ccteam_dir(&cc);
        let entries = mailbox.list_inbox()?;
        if entries.is_empty() {
            return Ok(());
        }
        let session = TmuxSession::from_name(state.tmux_session.clone());
        if !session.exists() {
            tracing::debug!(
                slug,
                session = %session.name(),
                "skipping inbox drain: tmux session not running yet",
            );
            return Ok(());
        }
        let progress_path = self.paths.progress_jsonl(slug);
        for path in entries {
            let msg = match InboxMessage::load(&path) {
                Ok(m) => m,
                Err(err) => {
                    tracing::warn!(
                        slug,
                        file = %path.display(),
                        error = %err,
                        "skip malformed inbox file (left in place for inspection)",
                    );
                    continue;
                }
            };
            let last = progress::last_event(&progress_path)?;
            let idle = progress::is_idle(last.as_ref());
            let body = msg.body.trim();
            if body.is_empty() {
                tracing::debug!(slug, file = %path.display(), "skip empty inbox body");
                let _ = std::fs::remove_file(&path);
                continue;
            }
            let message = progress::idle_aware_message(body, idle);
            if let Err(err) = session.send_keys(&message) {
                tracing::warn!(
                    slug,
                    file = %path.display(),
                    error = %err,
                    "send-keys failed; leaving inbox file for retry",
                );
                continue;
            }
            progress::append_event(
                &progress_path,
                &json!({
                    "ts": Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
                    "event": "inbox_consumed",
                    "session": session.name(),
                    "msg_file": path.file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or(""),
                    "source": msg.front.source,
                    "source_user": msg.front.source_user,
                    "idle": idle,
                }),
            )?;
            // Idempotent ack: delete after successful delivery.
            if let Err(err) = std::fs::remove_file(&path) {
                tracing::warn!(
                    slug,
                    file = %path.display(),
                    error = %err,
                    "could not delete consumed inbox file; channel adapter may resend",
                );
            }
        }
        Ok(())
    }

    /// Count phase-DAG projects whose tmux session is currently
    /// driving a phase (`InFlight` or `AutoLocked`). The concurrency
    /// gate (`MAX_CONCURRENT_PROJECTS`) compares this to the cap so
    /// over-the-limit idle projects wait their turn. Evergreen sessions
    /// are excluded — they're permanent fixtures in the User
    /// Interaction Layer, not phase-DAG workers (§6.4 candidate 5).
    pub fn count_active_regular(&self, projects: &[(String, ProjectState)]) -> usize {
        projects
            .iter()
            .filter(|(_, s)| !self.is_evergreen(&s.team))
            .filter(|(_, s)| {
                matches!(s.phase_state, PhaseState::InFlight | PhaseState::AutoLocked)
            })
            .count()
    }

    /// Run until `shutdown` resolves. Each tick + each progress event
    /// currently logs at debug level; later M0 tasks fill in the body.
    pub async fn run<F>(&self, shutdown: F) -> Result<()>
    where
        F: std::future::Future<Output = ()> + Send,
    {
        let progress_dir = self.paths.root.join("progress");
        std::fs::create_dir_all(&progress_dir)
            .with_context(|| format!("create {}", progress_dir.display()))?;

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<notify::Event>();

        // Keep the watcher alive for the duration of the loop.
        let mut watcher: RecommendedWatcher = notify::recommended_watcher(
            move |res: notify::Result<notify::Event>| match res {
                Ok(event) => {
                    let _ = tx.send(event);
                }
                Err(err) => tracing::warn!(?err, "progress watcher error"),
            },
        )?;
        watcher
            .watch(&progress_dir, RecursiveMode::NonRecursive)
            .with_context(|| format!("watch {}", progress_dir.display()))?;

        let mut tick = tokio::time::interval(self.config.tick_interval);
        // First tick fires immediately by default; skip it so observers can
        // assert "tick fired" without tolerating a 0-delay tick.
        tick.tick().await;

        // M0.23.1: heartbeat so MCP entrypoints / meta-agent skill can
        // surface "daemon down" via stat alone (no IPC). Fires at a
        // fixed cadence (`HEARTBEAT_INTERVAL`); supervisors allow a
        // grace of 2× before declaring the daemon dead.
        let mut heartbeat = tokio::time::interval(crate::daemon::HEARTBEAT_INTERVAL);
        // Touch immediately so a freshly-started daemon is observable
        // before its first 30s elapses.
        if let Err(err) = crate::daemon::write_heartbeat(&self.paths) {
            tracing::warn!(error = %err, "initial heartbeat write failed");
        }

        let mut tick_count: u64 = 0;
        let mut event_count: u64 = 0;

        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                _ = &mut shutdown => {
                    tracing::info!(
                        tick_count,
                        event_count,
                        "orchestrator shutdown signal received",
                    );
                    crate::daemon::remove_heartbeat(&self.paths);
                    return Ok(());
                }
                _ = tick.tick() => {
                    tick_count += 1;
                    self.poll_tick(tick_count).await?;
                }
                _ = heartbeat.tick() => {
                    if let Err(err) = crate::daemon::write_heartbeat(&self.paths) {
                        tracing::warn!(error = %err, "heartbeat write failed");
                    }
                }
                Some(event) = rx.recv() => {
                    event_count += 1;
                    self.handle_progress_event(event).await?;
                }
            }
        }
    }

    async fn poll_tick(&self, tick_count: u64) -> Result<()> {
        let projects = self.discover_projects()?;
        let active_regular = self.count_active_regular(&projects);
        let total_templates: usize =
            self.teams.values().map(|t| t.templates.len()).sum();
        tracing::debug!(
            tick = tick_count,
            teams = self.teams.len(),
            templates = total_templates,
            projects = projects.len(),
            active_regular,
            max_concurrent = MAX_CONCURRENT_PROJECTS,
            "orchestrator tick",
        );
        let now = Utc::now();

        // Process meta-agent projects first — they're permanent fixtures
        // and must not be deferred when the regular concurrency cap is
        // reached. Then handle regular projects with the cap.
        let mut regular_dispatch_budget =
            MAX_CONCURRENT_PROJECTS.saturating_sub(active_regular);

        for (slug, state) in projects {
            self.warn_if_stalled(&slug, &state, now);
            let state = match self.enforce_cost_thresholds(&slug, state)? {
                Some(updated) => updated,
                None => continue, // hard-kill terminated this project
            };

            if self.is_evergreen(&state.team) {
                // Evergreen sessions skip the concurrency cap — they're
                // permanent fixtures in the User Interaction Layer
                // (§6.4 candidate 5).
                if let Err(err) = self.process_project(&slug, state) {
                    tracing::error!(
                        slug,
                        error = format!("{err:#}"),
                        "evergreen tick failed",
                    );
                }
                continue;
            }

            // M1.2 concurrency gate: only let an idle regular project
            // *start* a new phase if the active count is under the cap.
            // Already-active projects (InFlight / AutoLocked) always run
            // through process_project so their AdvancePhase / Escalated
            // transitions still land.
            let already_active = matches!(
                state.phase_state,
                PhaseState::InFlight | PhaseState::AutoLocked,
            );
            if !already_active && regular_dispatch_budget == 0 {
                tracing::debug!(
                    slug,
                    "regular project queued: max_concurrent_projects ({}) reached",
                    MAX_CONCURRENT_PROJECTS,
                );
                continue;
            }

            match self.process_project(&slug, state) {
                Ok(updated) => {
                    if !already_active
                        && matches!(
                            updated.phase_state,
                            PhaseState::InFlight | PhaseState::AutoLocked,
                        )
                    {
                        regular_dispatch_budget = regular_dispatch_budget.saturating_sub(1);
                    }
                }
                Err(err) => tracing::error!(
                    slug,
                    error = format!("{err:#}"),
                    "project tick failed",
                ),
            }
        }
        Ok(())
    }

    /// Apply the cost-threshold ladder (tech-design §6.8): log soft /
    /// mid warnings, hard-kill the tmux session and mark the project
    /// escalated when `cost_used_usd > hard_kill_threshold_usd`.
    /// Returns `Ok(None)` when the project was hard-killed (caller
    /// should skip it for the rest of the tick).
    fn enforce_cost_thresholds(
        &self,
        slug: &str,
        mut state: ProjectState,
    ) -> Result<Option<ProjectState>> {
        use crate::team::CostPolicy;
        // V0.2 §6.4 candidate 5: declarative cost policy per team.
        // `CostPolicy::None` (evergreen meta-agent) bypasses entirely.
        // `CostPolicy::KillAt(threshold)` is the historical dev /
        // product-research path; `threshold = None` falls back to
        // `state.hard_kill_threshold_usd` (default $200 from M1).
        let policy = self.cost_policy(&state.team);
        if matches!(policy, CostPolicy::None) {
            return Ok(Some(state));
        }
        // V0.2.1 F28: project-layer override wins for terminal-state check.
        if self
            .team_runtime_for_state(&state)
            .is_some_and(|t| t.dag.is_terminal_state(&state))
        {
            return Ok(Some(state));
        }
        match cost::classify(&state) {
            CostLevel::Ok => {}
            CostLevel::SoftWarn => tracing::warn!(
                slug,
                cost = state.cost_used_usd,
                "cost ≥${:.0}: soft-warn threshold crossed",
                state.soft_warn_threshold_usd,
            ),
            CostLevel::MidWarn => tracing::error!(
                slug,
                cost = state.cost_used_usd,
                "cost ≥${:.0}: consider attaching",
                cost::COST_MID_WARN_USD,
            ),
            CostLevel::HardKill => {
                let hard = match policy {
                    CostPolicy::KillAt(Some(team_override)) => team_override,
                    _ => state.hard_kill_threshold_usd,
                };
                tracing::error!(
                    slug,
                    cost = state.cost_used_usd,
                    "cost > ${hard}: HARD KILL — terminating tmux session and escalating",
                );
                let session = TmuxSession::from_name(state.tmux_session.clone());
                if let Err(err) = session.kill() {
                    tracing::error!(slug, error = %err, "tmux kill failed during hard-kill");
                }
                state.phase_history.push(PhaseHistoryEntry {
                    phase: state.current_phase.clone(),
                    status: "escalated".into(),
                    duration_s: 0,
                    cost_usd: state.cost_used_usd,
                });
                state.phase_state = PhaseState::Idle;
                state.user_pause_pending = true;
                let esc_path = self.paths.project_ccteam_dir(slug).join("escalation.md");
                if let Some(parent) = esc_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let body = format!(
                    "# Escalation (cost hard kill)\n\nphase: {}\nreason: cost exceeded ${} hard limit (${:.2} used)\n\nrun `ccteam resume <slug>` only after diagnosing why claude was burning budget.\n",
                    state.current_phase, hard, state.cost_used_usd,
                );
                let _ = std::fs::write(&esc_path, body);
                state.save(&self.paths.project_state(slug))?;
                progress::append_event(
                    &self.paths.progress_jsonl(slug),
                    &json!({
                        "ts": Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
                        "event": "escalate",
                        "reason": format!(
                            "cost ${:.2} > hard limit ${}",
                            state.cost_used_usd, hard,
                        ),
                        "kind": "cost_hard_kill",
                    }),
                )?;
                return Ok(None);
            }
        }
        Ok(Some(state))
    }

    fn warn_if_stalled(&self, slug: &str, state: &ProjectState, now: chrono::DateTime<Utc>) {
        // Evergreen sessions sit idle by design (waiting for the next
        // NL message) — stall semantics don't apply. dag.is_terminal_state
        // would also return false for them since they have no phase
        // history, so guard explicitly. (§6.4 candidate 5 declarative
        // replacement.)
        if self.is_evergreen(&state.team) {
            return;
        }
        // V0.2.1 F28: project-layer override wins.
        let team_cow = self.team_runtime_for_state(state);
        let team: Option<&TeamRuntime> = team_cow.as_deref();
        if team.is_some_and(|t| t.dag.is_terminal_state(state)) {
            return;
        }
        let silent = stall::silent_seconds(state, now);

        // Per-phase thresholds: research's 04-primary phase legitimately
        // waits hours for human-supplied data, so applying dev's 5/15/30
        // hardcoded buckets there would fire false warnings every tick.
        // Falls back to the 5/15/30 default when the phase template
        // doesn't declare `stall_warn_minutes` (or current_phase is
        // empty during bootstrap).
        let thresholds = team
            .and_then(|t| t.templates.iter().find(|p| p.name == state.current_phase))
            .map(|t| StallThresholds::from_phase(t.stall_warn_minutes))
            .unwrap_or_default();

        match stall::classify_with_thresholds(silent, &thresholds) {
            StallLevel::Ok => {}
            StallLevel::Warn => tracing::warn!(
                slug,
                phase = %state.current_phase,
                silent_seconds = silent,
                threshold_minutes = thresholds.warn_minutes(),
                "stall ≥{}min: project quiet, no progress events",
                thresholds.warn_minutes(),
            ),
            StallLevel::Suspicious => tracing::error!(
                slug,
                phase = %state.current_phase,
                silent_seconds = silent,
                threshold_minutes = thresholds.suspicious_minutes(),
                "stall ≥{}min: claude may be hung, consider attaching",
                thresholds.suspicious_minutes(),
            ),
            StallLevel::Escalate => tracing::error!(
                slug,
                phase = %state.current_phase,
                silent_seconds = silent,
                threshold_minutes = thresholds.escalate_minutes(),
                "stall ≥{}min: ESCALATE territory; M1 telegram should ping user",
                thresholds.escalate_minutes(),
            ),
        }
    }

    async fn handle_progress_event(&self, event: notify::Event) -> Result<()> {
        // M0.8/M0.9 will tail the changed JSONL file and run the state
        // machine. For now just log so we can confirm the watcher is
        // wired up.
        tracing::debug!(?event, "progress event");
        Ok(())
    }
}

/// Compose the "当前进度" summary the orchestrator appends to a
/// project's CLAUDE.md before recycling the tmux session. Pure: only
/// reads `state` so it's trivially unit-testable.
pub fn build_progress_summary(state: &ProjectState) -> String {
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let mut s = String::new();
    s.push_str(&format!("\n## 当前进度 (context reset @ {now})\n\n"));
    s.push_str(&format!("- 当前 phase: {}\n", state.current_phase));
    s.push_str(&format!("- 累计 cost: ${:.2}\n", state.cost_used_usd));
    s.push_str(&format!(
        "- 已完成 reset 次数: {}\n",
        state.context_reset_count + 1
    ));
    if state.phase_history.is_empty() {
        s.push_str("- phase 历史: (空)\n");
    } else {
        s.push_str("- phase 历史:\n");
        for h in &state.phase_history {
            s.push_str(&format!("  - {} ({})\n", h.phase, h.status));
        }
    }
    s.push('\n');
    s.push_str("> 这一节由 ccteam 在 context 接近上限时自动追加。新 session 启动后请按当前 phase 继续工作。\n");
    s
}

/// Append the summary to `claude_md`, creating the file with a header
/// if it didn't exist.
pub fn append_progress_summary(claude_md: &Path, summary: &str) -> Result<()> {
    use std::io::Write;
    if let Some(parent) = claude_md.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create {}", parent.display()))?;
    }
    if !claude_md.exists() {
        std::fs::write(
            claude_md,
            "# CLAUDE.md (auto-managed by ccteam)\n\n本文件由 ccteam 自动维护;不要手改\"当前进度\"节,会被覆盖。\n",
        )
        .with_context(|| format!("create {}", claude_md.display()))?;
    }
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(claude_md)
        .with_context(|| format!("open {}", claude_md.display()))?;
    f.write_all(summary.as_bytes())?;
    Ok(())
}

fn wait_for_ready(ready_path: &Path, timeout: Duration) -> Result<()> {
    use std::time::Instant;
    let start = Instant::now();
    while start.elapsed() < timeout {
        if ready_path.exists() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    anyhow::bail!(
        "timeout after {:?} waiting for {}",
        timeout,
        ready_path.display(),
    );
}

/// M0.5.3: refuse to start when any shipped phase template asks for a
/// tool (subagent, skill, or MCP server) that isn't reachable on this
/// machine. The error message lists every miss with a concrete fix
/// command so the operator doesn't have to guess what's broken.
///
/// Pulled out of `Orchestrator::new` so tests and `ccteam doctor
/// --tool-surface` can share the cross-check.
pub fn check_phase_tools(templates: &[PhaseTemplate]) -> Result<()> {
    let snap = match user_claude_dir() {
        Ok(dir) => ToolSurfaceSnapshot::scan(&dir).unwrap_or_default(),
        Err(err) => {
            tracing::warn!(
                error = %err,
                "could not resolve ~/.claude/ for tool-surface check; only built-ins available",
            );
            ToolSurfaceSnapshot::default()
        }
    };
    let mut all_missing: Vec<(String, crate::tool_surface::MissingTool)> = Vec::new();
    for t in templates {
        for m in t.missing_tools_against(&snap) {
            all_missing.push((t.name.clone(), m));
        }
    }
    if all_missing.is_empty() {
        return Ok(());
    }
    let mut msg = String::from(
        "phase template tool surface check failed — at least one phase requires a tool that is not reachable:\n",
    );
    for (phase, m) in &all_missing {
        msg.push_str(&format!(
            "  - phase `{phase}` needs {kind} `{name}` — {hint}\n",
            kind = m.kind(),
            name = m.name(),
            hint = m.fix_hint(),
        ));
    }
    msg.push_str(
        "\nrun `ccteam doctor --tool-surface` for the full report, or pass `--skip-tool-check` to start anyway.",
    );
    Err(anyhow::anyhow!(msg))
}

fn load_phase_templates(dir: &Path) -> Result<Vec<PhaseTemplate>> {
    if !dir.exists() {
        tracing::warn!(
            phases_dir = %dir.display(),
            "phases directory absent — orchestrator running with no templates",
        );
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("read_dir {}", dir.display()))?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("md"))
        })
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for e in entries {
        let path = e.path();
        let template = PhaseTemplate::load(&path)
            .with_context(|| format!("phase template {}", path.display()))?;
        out.push(template);
    }
    Ok(out)
}

/// M3.3: discover and load every team registered under
/// `<root>/teams/<name>/team.yaml`. Returns a map keyed by team name.
///
/// Legacy behavior: if no `<root>/teams/dev/team.yaml` exists but
/// `<root>/phases/` does, register an implicit dev team with default
/// `TeamSpec` so M0–M2 installs without `ccteam init` keep working.
///
/// Each team's templates are validated with `validate_m0` so a broken
/// phase YAML on disk fails-fast at orchestrator startup. If a team's
/// `phase_dir` directory is missing we log + skip the team (rather
/// than failing the whole orchestrator) — adding a team.yaml to an
/// otherwise empty install is a normal step.
fn load_team_runtimes(paths: &CcteamPaths) -> Result<HashMap<String, TeamRuntime>> {
    use crate::team_resolver::{
        default_user_staging_dir, discover_team_names, resolve_team, TeamResolveContext,
        TEAM_SOURCES,
    };

    let mut teams = HashMap::new();
    let teams_dir = paths.root.join("teams");

    let dev_yaml = teams_dir.join("dev").join("team.yaml");
    let dev_yaml_present = dev_yaml.exists();

    // V0.2 M0.17.4: walk every team name discoverable across User +
    // Repo layers, then resolve through the layered resolver so a
    // user-staged override beats a shipped seed of the same name.
    let user_staging = default_user_staging_dir();
    let ctx = TeamResolveContext::for_orchestrator(&paths.root, &user_staging);
    let team_names = discover_team_names(&ctx);
    for name in team_names {
        let spec = match resolve_team(&name, &ctx) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(
                    team = %name,
                    error = format!("{err:#}"),
                    "team listed by discover_team_names but resolve_team failed; skipping",
                );
                continue;
            }
        };
        // The TeamSource that won resolution is also where the
        // phase markdowns live (per-team-dir layout, M0.17.2).
        // Compute its team_dir by re-walking sources to find the
        // first hit — keeps the resolver pure (no path returned).
        let team_dir = TEAM_SOURCES
            .iter()
            .filter_map(|s| s.path_for(&name, &ctx))
            .find(|p| p.exists())
            .and_then(|p| p.parent().map(Path::to_path_buf));

        // V0.2 §6.4 candidate 5: evergreen teams (meta-agent) ship
        // an empty phase set — they're event-loop sessions, not
        // phase-DAG ones. Skip the phase_dir presence check + the
        // template load; the resulting TeamRuntime carries an
        // empty templates Vec and an empty DAG, which `decide_tick`
        // handles via early NoOp.
        let (templates, dag) = if spec.evergreen {
            let dag = Dag::from_templates(&[]).with_context(|| {
                format!("team `{}` build empty phase DAG", spec.name)
            })?;
            (Vec::new(), dag)
        } else {
            // V0.2 M0.17.2: phase_dir is relative to the team
            // directory. Legacy `phases-<team>` values were rewritten
            // to `phases` by TeamSpec::parse so this join lands at
            // the new layout regardless of yaml vintage.
            let Some(team_dir) = team_dir else {
                tracing::warn!(
                    team = %spec.name,
                    "non-evergreen team has no resolvable team_dir; \
                     run `ccteam doctor --reset-shipped-teams`",
                );
                continue;
            };
            let phase_dir = team_dir.join(&spec.phase_dir);
            if !phase_dir.exists() {
                tracing::warn!(
                    team = %spec.name,
                    phase_dir = %phase_dir.display(),
                    "team registered but phase_dir is missing; \
                     run `ccteam init` to populate templates",
                );
                continue;
            }
            let templates = load_phase_templates(&phase_dir)?;
            for t in &templates {
                t.validate_m0().with_context(|| {
                    format!(
                        "team `{}` phase template `{}` failed M0 validation",
                        spec.name, t.name,
                    )
                })?;
            }
            let dag = Dag::from_templates(&templates).with_context(|| {
                format!("team `{}` build phase DAG", spec.name)
            })?;
            (templates, dag)
        };
        teams.insert(
            name,
            TeamRuntime {
                spec,
                templates,
                dag,
            },
        );
    }
    // Legacy fallback for dev: if no teams/dev/team.yaml on disk but
    // phases/ has templates, register an implicit dev team so M0–M2
    // installs without `ccteam init` keep running.
    if !dev_yaml_present && !teams.contains_key("dev") {
        let phase_dir = paths.phases_dir();
        if phase_dir.exists() {
            let templates = load_phase_templates(&phase_dir)?;
            for t in &templates {
                t.validate_m0().with_context(|| {
                    format!("legacy dev phase template `{}` failed M0 validation", t.name)
                })?;
            }
            if !templates.is_empty() {
                let dag = Dag::from_templates(&templates)
                    .context("legacy dev: build phase DAG")?;
                let spec = TeamSpec {
                    name: "dev".into(),
                    aliases: Vec::new(),
                    description: "Software development team (legacy fallback)".into(),
                    retro_schema: Vec::new(),
                    critic_dimensions: Vec::new(),
                    escalate_grammar_extensions: Vec::new(),
                    golden_rules: crate::team::TeamGoldenRules::default(),
                    phase_dir: "phases".into(),
                    verdict_schema: Vec::new(),
                    evergreen: false,
                    cost_policy: crate::team::CostPolicy::default(),
                    claude_md_template: String::new(),
                };
                teams.insert(
                    "dev".into(),
                    TeamRuntime {
                        spec,
                        templates,
                        dag,
                    },
                );
            }
        }
    }

    if teams.is_empty() {
        tracing::warn!(
            root = %paths.root.display(),
            "no teams registered — orchestrator inert until phases/ or teams/ populated",
        );
    }
    Ok(teams)
}

/// V0.2.1 F28 — rebuild a single team's runtime from a project-layer
/// override. Called per-tick (no caching) so a project can edit its
/// `<project_dir>/.ccteam/team/team.yaml` without restarting the
/// orchestrator. The override's `phase_dir` is resolved relative to the
/// project's `team_dir` (`<project_dir>/.ccteam/team/`); when the
/// override doesn't ship phase markdowns, the resolution falls back
/// through TEAM_SOURCES so user / repo phase templates still drive the
/// DAG (the override is mainly for spec-level fields like
/// `golden_rules` / `description`).
fn build_project_team_runtime(
    paths: &CcteamPaths,
    state: &ProjectState,
    project_dir: &Path,
) -> Result<TeamRuntime> {
    use crate::team_resolver::{
        default_user_staging_dir, resolve_team, TeamResolveContext, TEAM_SOURCES,
    };

    let user_staging = default_user_staging_dir();
    let ctx = TeamResolveContext::for_orchestrator(&paths.root, &user_staging)
        .with_project(project_dir);

    let spec = resolve_team(&state.team, &ctx)
        .with_context(|| format!("project-layer resolve team `{}`", state.team))?;

    if spec.evergreen {
        let dag = Dag::from_templates(&[]).with_context(|| {
            format!("team `{}` build empty phase DAG (project layer)", spec.name)
        })?;
        return Ok(TeamRuntime {
            spec,
            templates: Vec::new(),
            dag,
        });
    }

    // Locate phase_dir on disk via the same priority order the
    // resolver used for the spec. The override's team_dir is
    // `<project_dir>/.ccteam/team/`; if the override only carries
    // a `team.yaml` (no phases/ subdir), fall through to user/repo
    // so phase markdowns still load.
    let team_dir = TEAM_SOURCES
        .iter()
        .filter_map(|s| s.path_for(&state.team, &ctx))
        .find(|p| p.exists())
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "project-layer team `{}` resolved spec but no team_dir found",
                state.team,
            )
        })?;
    let phase_dir = team_dir.join(&spec.phase_dir);
    let templates = if phase_dir.exists() {
        load_phase_templates(&phase_dir)?
    } else {
        // Project override carried just team.yaml; reuse user/repo
        // phase markdowns by walking sources skipping the project layer.
        let fallback_ctx =
            TeamResolveContext::for_orchestrator(&paths.root, &user_staging);
        let fallback_team_dir = TEAM_SOURCES
            .iter()
            .filter_map(|s| s.path_for(&state.team, &fallback_ctx))
            .find(|p| p.exists())
            .and_then(|p| p.parent().map(Path::to_path_buf));
        match fallback_team_dir {
            Some(d) => {
                let pd = d.join(&spec.phase_dir);
                if pd.exists() {
                    load_phase_templates(&pd)?
                } else {
                    Vec::new()
                }
            }
            None => Vec::new(),
        }
    };
    for t in &templates {
        t.validate_m0().with_context(|| {
            format!(
                "project-layer team `{}` template `{}` failed M0 validation",
                spec.name, t.name,
            )
        })?;
    }
    let dag = Dag::from_templates(&templates).with_context(|| {
        format!("project-layer team `{}` build phase DAG", spec.name)
    })?;
    Ok(TeamRuntime {
        spec,
        templates,
        dag,
    })
}

/// M3.6: static intersection of `open_decisions` (outbox basenames the
/// previous phase declared as deferred) against `required_inputs`
/// (the next phase's input list — typically project-relative paths).
/// Returns the basenames that block the advance.
///
/// The match is "any required_input whose path basename or whole-string
/// equals an open_decision". So a `required_inputs:` entry of
/// `.ccteam/outbox/clarify-X.md` matches an open_decision of
/// `clarify-X.md`. Direct name matches also work for phases that list
/// `clarify-X.md` without a path.
pub fn intersect_open_decisions_with_required_inputs(
    open_decisions: &[String],
    required_inputs: &[String],
) -> Vec<String> {
    let mut blocking = Vec::new();
    for od in open_decisions {
        for ri in required_inputs {
            let base = std::path::Path::new(ri)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(ri.as_str());
            if base == od.as_str() || ri == od {
                if !blocking.iter().any(|b: &String| b == od) {
                    blocking.push(od.clone());
                }
                break;
            }
        }
    }
    blocking
}

#[cfg(test)]
mod config_tests {
    use super::*;

    #[test]
    fn default_claude_argv_enables_1m_context() {
        // M0.23.2: production claude session must opt into the 1M
        // context window (`<model>[1m]` Claude Code suffix). tech-design
        // §6.1 / §6.9 require this for cache reuse + the 60% reset budget.
        let cfg = OrchestratorConfig::default();
        let argv = &cfg.claude_argv;
        assert_eq!(argv.first().map(String::as_str), Some("claude"));
        assert!(
            argv.iter().any(|a| a == "--dangerously-skip-permissions"),
            "default argv must keep --dangerously-skip-permissions: {argv:?}",
        );
        let model_idx = argv
            .iter()
            .position(|a| a == "--model")
            .expect("default argv must pass --model");
        let model = argv
            .get(model_idx + 1)
            .expect("--model must be followed by a value");
        assert!(
            model.ends_with("[1m]"),
            "default model must opt into 1M context (got `{model}`); the `[1m]` \
             suffix is the Claude Code documented 1M alias",
        );
        assert_eq!(model, DEFAULT_CLAUDE_MODEL);
    }
}
