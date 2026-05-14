//! V0.4.0 F60 — Thin orchestrator stub.
//!
//! The legacy phase state machine (M0–M3.x; ~2700 LOC) was deleted in
//! F60 along with `phases.rs`, `golden_rules.rs`, `dag.rs`, and the
//! template loaders. This module is intentionally minimal until F66
//! lands the artifact-trigger workflow loop.
//!
//! Public surface kept stable for callers that already exist:
//! - `Orchestrator { paths }` (constructor + path accessor)
//! - `OrchestratorConfig` (preserved for CLI flag plumbing in
//!   `ccteam start`; fields trimmed to what the stub needs)
//! - `MAX_CONCURRENT_PROJECTS` (consumed by `discover_projects` callers)
//! - `DEFAULT_CLAUDE_MODEL` (consumed by CLI + tests for `--model`)
//! - `run_project` / `run` — return `todo!("F66 thin orchestrator")`
//!
//! Everything phase-related (decide_tick, dispatch_phase,
//! attachments_for_next_phase, handle_golden_rules_violation, the
//! TeamRuntime { templates, dag } cache, all PhaseState handling) has
//! been removed. F63 introduces the new `workflow.yaml` surface; F66
//! re-implements the dispatch loop against it.

use std::time::Duration;

use anyhow::Result;

use crate::paths::CcteamPaths;

/// M1.2: how many regular project sessions can run concurrently. Hard-
/// coded for M1; M3 may move it to `team.yaml` / global config. The
/// meta-agent session is **not counted** — it's a permanent fixture in
/// the User Interaction Layer.
pub const MAX_CONCURRENT_PROJECTS: usize = 3;

/// Production model identifier passed to `claude --model`. The `[1m]`
/// suffix is Claude Code's documented opt-in to the 1M-token context
/// window. tech-design §6.1 / §6.9 require the long context for cache
/// reuse + the 60% phase-boundary reset budget.
///
/// V0.2 §7 / dev-plan §9 M0.23.2: 1M default. When Anthropic publishes
/// a newer Sonnet alias, change this single line.
pub const DEFAULT_CLAUDE_MODEL: &str = "claude-sonnet-4-6[1m]";

#[derive(Debug, Clone)]
pub struct OrchestratorConfig {
    /// How often the main loop ticks in the absence of progress events.
    pub tick_interval: Duration,
    /// argv for the in-pane process when (re)starting a project's tmux
    /// session. Default is `claude --dangerously-skip-permissions`.
    pub claude_argv: Vec<String>,
    /// How long the context-reset routine waits for SessionStart's
    /// ready marker before bailing.
    pub ready_timeout: Duration,
    /// Extra delay between SessionStart's ready marker landing and the
    /// first send-keys.
    pub post_ready_warmup: Duration,
    /// Skip the M0.5.3 startup `tools_required` check (F66 will revisit
    /// the validator contract against the new workflow schema).
    pub skip_tool_check: bool,
}

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

/// V0.4.0 F60 stub. The full orchestrator is rebuilt in F66 against
/// the new `workflow.yaml` schema (F63) and artifact-trigger watcher
/// (F64). For now the type exists so `ccteam-cli` keeps compiling but
/// every dispatch / run entry point returns a `todo!()` so a stray
/// production call is loud, not silent.
#[derive(Debug)]
pub struct Orchestrator {
    paths: CcteamPaths,
    #[allow(dead_code)]
    config: OrchestratorConfig,
}

impl Orchestrator {
    /// Construct the stub. Stores `paths` and `config`; no side effects
    /// on `~/.ccteam/`, no validation of phase templates (the legacy
    /// validator was deleted with the rest of the phase machinery).
    pub fn new(paths: CcteamPaths, config: OrchestratorConfig) -> Result<Self> {
        Ok(Self { paths, config })
    }

    pub fn paths(&self) -> &CcteamPaths {
        &self.paths
    }

    /// F66 will lay down the per-project artifact-trigger loop against
    /// the new `workflow.yaml` schema. Until then the stub refuses to
    /// pretend it can run anything.
    pub async fn run_project(&self, _slug: &str) -> Result<()> {
        todo!("F66 thin orchestrator");
    }

    /// F66 will lay down the daemon loop (watchdog + artifact-trigger
    /// dispatch + meta-agent inbox drain). Until then the stub refuses
    /// to pretend it can run anything.
    pub async fn run<F>(&self, _shutdown: F) -> Result<()>
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        todo!("F66 thin orchestrator");
    }
}
