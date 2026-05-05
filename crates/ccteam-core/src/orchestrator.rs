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
use crate::paths::CcteamPaths;
use crate::phases::PhaseTemplate;
use crate::progress;
use crate::stall::{self, StallLevel, StallThresholds};
use crate::state::{PhaseHistoryEntry, PhaseState, ProjectState};
use crate::tmux::TmuxSession;
use crate::tool_surface::{user_claude_dir, ToolSurfaceSnapshot};

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
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            tick_interval: Duration::from_secs(30),
            claude_argv: vec!["claude".into(), "--dangerously-skip-permissions".into()],
            ready_timeout: Duration::from_secs(60),
            post_ready_warmup: Duration::from_secs(3),
            skip_tool_check: false,
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
        let progress_path = self.paths.progress_jsonl(slug);
        let last = progress::last_event(&progress_path)?;
        let idle = progress::is_idle(last.as_ref());

        let prompt = progress::build_phase_prompt(phase);
        let message = progress::idle_aware_message(&prompt, idle);

        let session = TmuxSession::for_slug(slug);
        session
            .send_keys(&message)
            .with_context(|| format!("send phase prompt to ccteam-{slug}"))?;

        let event = json!({
            "ts": Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            "event": "phase_inject",
            "phase": phase,
            "idle": idle,
        });
        progress::append_event(&progress_path, &event)?;
        Ok(())
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

        let session = TmuxSession::for_slug(slug);
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
        if self.dag.is_terminal_state(state) {
            return Ok(());
        }
        let session = TmuxSession::for_slug(slug);
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
    pub fn process_project(&self, slug: &str, mut state: ProjectState) -> Result<ProjectState> {
        const MAX_ITERS: u32 = 4;
        let progress_path = self.paths.progress_jsonl(slug);
        let state_path = self.paths.project_state(slug);

        for _ in 0..MAX_ITERS {
            let events = progress::read_all_events(&progress_path)?;
            let action = decide_tick_from_events(&self.dag, &state, &events);
            match action {
                TickAction::NoOp => return Ok(state),
                TickAction::AdvancePhase { from, to } => {
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
        tracing::debug!(
            tick = tick_count,
            templates = self.templates.len(),
            projects = projects.len(),
            "orchestrator tick",
        );
        let now = Utc::now();
        for (slug, state) in projects {
            // Stall + cost classification first so we observe the
            // pre-action state. enforce_cost_thresholds may mutate
            // state and return Some(updated) when it does.
            self.warn_if_stalled(&slug, &state, now);
            let state = match self.enforce_cost_thresholds(&slug, state)? {
                Some(updated) => updated,
                None => continue, // hard-kill terminated this project
            };
            if let Err(err) = self.process_project(&slug, state) {
                tracing::error!(slug, error = format!("{err:#}"), "project tick failed");
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
                let session = TmuxSession::for_slug(slug);
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
