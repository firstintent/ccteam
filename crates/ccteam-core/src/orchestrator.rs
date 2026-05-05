//! ccteam orchestrator main loop. M0 wires the bare-bones daemon:
//!
//! - load + M0-validate every phase template under `~/.ccteam/phases/`
//!   on startup (fail-fast on `parallelism != solo` per development-plan
//!   §2.1 M0.6 acceptance);
//! - 30s tick (configurable for tests);
//! - notify-rs watcher on `~/.ccteam/progress/` so the loop wakes when
//!   a hook appends an event;
//! - cancellable run via a caller-supplied shutdown future.
//!
//! Per-tick + per-event handling are stubs; M0.7 fills in tmux session
//! lifecycle, M0.8 the idle-aware injection, M0.9 the state machine,
//! M0.10 the context-reset bridge, M0.13/M0.14 stall + cost.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::{json, Value};

use crate::cost::{self, CostLevel};
use crate::dag::Dag;
use crate::fix_loop::{self, FixLoopState};
use crate::inbox::{InboxMessage, SessionMailbox};
use crate::meta_agent::META_TEAM_NAME;
use crate::paths::CcteamPaths;
use crate::phases::{PhaseTemplate, SubSkillTrigger};
use crate::progress;
use crate::stall::{self, StallLevel, StallThresholds};
use crate::state::{PhaseHistoryEntry, PhaseState, ProjectState};
use crate::subskill::{self, ClaudePRunner, SubSkillRunner};
use crate::tmux::TmuxSession;
use crate::tool_surface::{user_claude_dir, ToolSurfaceSnapshot};

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

    if matches!(
        state.phase_state,
        PhaseState::InFlight | PhaseState::FixLocked
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

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            tick_interval: Duration::from_secs(30),
            claude_argv: vec!["claude".into(), "--dangerously-skip-permissions".into()],
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
    templates: Vec<PhaseTemplate>,
    dag: Dag,
}

impl Orchestrator {
    /// Construct + validate. Returns `Err` if any shipped phase template
    /// fails M0 validation, so the daemon refuses to start in a known-bad
    /// state instead of silently routing through agent_team / multi_session
    /// paths that aren't implemented yet.
    pub fn new(paths: CcteamPaths, config: OrchestratorConfig) -> Result<Self> {
        let templates = load_phase_templates(&paths.phases_dir())?;
        for t in &templates {
            t.validate_m0()
                .with_context(|| format!("phase template `{}` failed M0 validation", t.name))?;
        }
        if !config.skip_tool_check {
            check_phase_tools(&templates)?;
        } else {
            tracing::warn!("skip_tool_check=true: phase tools_required not validated");
        }
        let dag = Dag::from_templates(&templates)
            .context("build phase DAG from loaded templates")?;
        tracing::info!(
            templates = templates.len(),
            phases_dir = %paths.phases_dir().display(),
            entry_phase = %dag.entry(),
            "orchestrator initialized",
        );
        Ok(Self {
            paths,
            config,
            templates,
            dag,
        })
    }

    pub fn templates(&self) -> &[PhaseTemplate] {
        &self.templates
    }

    pub fn dag(&self) -> &Dag {
        &self.dag
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
        let prompt = progress::build_phase_prompt_with_attachments(phase, &attachment_refs);
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
    /// the phase template doesn't exist (e.g. dropped from the team)
    /// or no sub-skills wrote anything, returns an empty vec. Public
    /// for direct testing (the dispatch path that consumes this is
    /// hard to drive in unit tests because of the tmux dependency).
    pub fn attachments_for_next_phase(
        &self,
        slug: &str,
        state: &ProjectState,
    ) -> Vec<String> {
        let Some(prev) = state.phase_history.last() else {
            return Vec::new();
        };
        let Some(template) = self.templates.iter().find(|t| t.name == prev.phase) else {
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
        // Meta-agent sessions never reach a terminal phase state and the
        // dag check below would skip them — guard meta-agent first so it
        // always tries to come up.
        let is_meta = state.team == META_TEAM_NAME;
        if !is_meta && self.dag.is_terminal_state(state) {
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
    /// Meta-agent projects (`team == META_TEAM_NAME`) bypass the phase
    /// DAG entirely — they're event-loop sessions, not phase-DAG ones —
    /// and only get `ensure_session` lifecycle handling. See
    /// `process_meta_project` for that path.
    pub fn process_project(&self, slug: &str, mut state: ProjectState) -> Result<ProjectState> {
        if state.team == META_TEAM_NAME {
            return self.process_meta_project(slug, state);
        }
        const MAX_ITERS: u32 = 4;
        let progress_path = self.paths.progress_jsonl(slug);
        let state_path = self.paths.project_state(slug);

        for _ in 0..MAX_ITERS {
            let events = progress::read_all_events(&progress_path)?;
            let action = decide_tick_from_events(&self.dag, &state, &events);
            match action {
                TickAction::NoOp => return Ok(state),
                TickAction::AdvancePhase { from, to } => {
                    // M2.1: phase_done sub-skills run *before* state
                    // advance so their output files are on disk by the
                    // time the next phase prompt is built (the prompt
                    // builder pulls in @-attachments from the prior
                    // phase's sub_skills outputs).
                    if let Some(prev_template) =
                        self.templates.iter().find(|t| t.name == from)
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
                    if !self.dag.is_terminal_state(&state)
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
                    if let Some(template) = self.templates.iter().find(|t| t.name == phase) {
                        self.run_phase_sub_skills(
                            slug,
                            template,
                            SubSkillTrigger::PhaseStart,
                        );
                    }
                    self.dispatch_phase(slug, &phase)?;
                    let template = self.templates.iter().find(|t| t.name == phase);
                    let target_state = if template.is_some_and(|t| t.auto_loop) {
                        // Stop hook (M0.12) drives the loop; orchestrator
                        // only re-enters on phase_done/escalate.
                        let t = template.expect("auto_loop branch implies template present");
                        let project_dir = self.paths.project_dir(slug);
                        let prompt = progress::build_phase_prompt(&phase);
                        let fl = FixLoopState::new(
                            slug.to_string(),
                            prompt,
                            t.auto_loop_max_iterations,
                            t.completion_signal.clone(),
                        );
                        fix_loop::write(&fix_loop::path_in(&project_dir), &fl)?;
                        PhaseState::FixLocked
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
        debug_assert_eq!(state.team, META_TEAM_NAME);
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

    /// Count regular (non-meta) projects whose tmux session is
    /// currently driving a phase (`InFlight` or `FixLocked`). The
    /// concurrency gate (`MAX_CONCURRENT_PROJECTS`) compares this to
    /// the cap so over-the-limit idle projects wait their turn.
    pub fn count_active_regular(projects: &[(String, ProjectState)]) -> usize {
        projects
            .iter()
            .filter(|(_, s)| s.team != META_TEAM_NAME)
            .filter(|(_, s)| {
                matches!(s.phase_state, PhaseState::InFlight | PhaseState::FixLocked)
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
                    return Ok(());
                }
                _ = tick.tick() => {
                    tick_count += 1;
                    self.poll_tick(tick_count).await?;
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
        let active_regular = Self::count_active_regular(&projects);
        tracing::debug!(
            tick = tick_count,
            templates = self.templates.len(),
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

            if state.team == META_TEAM_NAME {
                if let Err(err) = self.process_project(&slug, state) {
                    tracing::error!(
                        slug,
                        error = format!("{err:#}"),
                        "meta tick failed",
                    );
                }
                continue;
            }

            // M1.2 concurrency gate: only let an idle regular project
            // *start* a new phase if the active count is under the cap.
            // Already-active projects (InFlight / FixLocked) always run
            // through process_project so their AdvancePhase / Escalated
            // transitions still land.
            let already_active = matches!(
                state.phase_state,
                PhaseState::InFlight | PhaseState::FixLocked,
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
                            PhaseState::InFlight | PhaseState::FixLocked,
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
        // Meta-agent sessions are evergreen and don't fit "hard kill on
        // budget" semantics — their cost is the user's running tab, not
        // a per-project budget. M1 lets them through; M4 may want a
        // separate ladder when conversation continuity (M4.6) lands.
        if state.team == META_TEAM_NAME {
            return Ok(Some(state));
        }
        if self.dag.is_terminal_state(&state) {
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
                let hard = state.hard_kill_threshold_usd;
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
        // Meta-agent sessions sit idle by design (waiting for the next
        // NL message) — stall semantics don't apply. dag.is_terminal_state
        // would also return false for them since they have no phase
        // history, so guard explicitly.
        if state.team == META_TEAM_NAME {
            return;
        }
        if self.dag.is_terminal_state(state) {
            return;
        }
        let silent = stall::silent_seconds(state, now);

        // Per-phase thresholds: research's 04-primary phase legitimately
        // waits hours for human-supplied data, so applying dev's 5/15/30
        // hardcoded buckets there would fire false warnings every tick.
        // Falls back to the 5/15/30 default when the phase template
        // doesn't declare `stall_warn_minutes` (or current_phase is
        // empty during bootstrap).
        let thresholds = self
            .templates
            .iter()
            .find(|t| t.name == state.current_phase)
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
