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

use crate::paths::CcteamPaths;
use crate::phases::PhaseTemplate;
use crate::progress;
use crate::state::{PhaseHistoryEntry, PhaseState, ProjectState};
use crate::tmux::TmuxSession;

/// M0 linear phase DAG. M2+ widens to fork on test results, sub-skill
/// scheduling, and (M3) multi-session fan-out / fan-in.
pub const M0_PHASE_DAG: &[(&str, Option<&str>)] = &[
    ("plan-eng", Some("implement")),
    ("implement", Some("test-author")),
    ("test-author", Some("test-run")),
    ("test-run", Some("fix")),
    ("fix", Some("ship")),
    ("ship", None),
];

pub const FIRST_PHASE: &str = "plan-eng";

/// Next phase per M0 DAG, or `None` if `current` is terminal / unknown.
pub fn next_phase(current: &str) -> Option<&'static str> {
    M0_PHASE_DAG
        .iter()
        .find(|(p, _)| *p == current)
        .and_then(|(_, n)| *n)
}

/// True if the project has reached a terminal state — `ship` passed or
/// any phase escalated. Both block automatic advance until the user
/// (M0: manual; M1: telegram) decides what to do next.
pub fn is_terminal(state: &ProjectState) -> bool {
    state
        .phase_history
        .iter()
        .any(|h| (h.phase == "ship" && h.status == "passed") || h.status == "escalated")
}

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

pub fn decide_tick(state: &ProjectState, last_event: Option<&Value>) -> TickAction {
    if is_terminal(state) {
        return TickAction::NoOp;
    }

    if state.phase_state == PhaseState::InFlight {
        let Some(e) = last_event else {
            return TickAction::NoOp; // dispatched but no events yet
        };
        let kind = e.get("event").and_then(|s| s.as_str()).unwrap_or("");
        match kind {
            "phase_done" => {
                let phase = e.get("phase").and_then(|s| s.as_str()).unwrap_or("");
                if phase == state.current_phase {
                    return TickAction::AdvancePhase {
                        from: state.current_phase.clone(),
                        to: next_phase(&state.current_phase).map(String::from),
                    };
                }
                // Stale phase_done event (e.g., from a prior phase that
                // re-ran). Don't act on it.
                TickAction::NoOp
            }
            "escalate" => {
                let reason = e
                    .get("reason")
                    .and_then(|s| s.as_str())
                    .unwrap_or("(no reason given)")
                    .to_string();
                TickAction::Escalated {
                    phase: state.current_phase.clone(),
                    reason,
                }
            }
            _ => TickAction::NoOp, // still busy
        }
    } else {
        // Idle: dispatch the current phase, defaulting to FIRST_PHASE.
        let phase = if state.current_phase.is_empty() {
            FIRST_PHASE.to_string()
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
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            tick_interval: Duration::from_secs(30),
            claude_argv: vec!["claude".into(), "--dangerously-skip-permissions".into()],
            ready_timeout: Duration::from_secs(60),
        }
    }
}

#[derive(Debug)]
pub struct Orchestrator {
    paths: CcteamPaths,
    config: OrchestratorConfig,
    templates: Vec<PhaseTemplate>,
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
        tracing::info!(
            templates = templates.len(),
            phases_dir = %paths.phases_dir().display(),
            "orchestrator initialized",
        );
        Ok(Self {
            paths,
            config,
            templates,
        })
    }

    pub fn templates(&self) -> &[PhaseTemplate] {
        &self.templates
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

    /// Apply `decide_tick` to one project, looping the AdvancePhase →
    /// DispatchPhase chain so a phase boundary doesn't take two ticks
    /// to act on. The loop iteration cap is a paranoia guard.
    pub fn process_project(&self, slug: &str, mut state: ProjectState) -> Result<ProjectState> {
        const MAX_ITERS: u32 = 4;
        let progress_path = self.paths.progress_jsonl(slug);
        let state_path = self.paths.project_state(slug);

        for _ in 0..MAX_ITERS {
            let last = progress::last_event(&progress_path)?;
            let action = decide_tick(&state, last.as_ref());
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
                    if !is_terminal(&state)
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
                    self.dispatch_phase(slug, &phase)?;
                    state.current_phase = phase;
                    state.phase_state = PhaseState::InFlight;
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
        for (slug, state) in projects {
            if let Err(err) = self.process_project(&slug, state) {
                tracing::error!(slug, error = %err, "project tick failed");
            }
        }
        Ok(())
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
