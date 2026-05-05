//! `ccteam` binary entry point. Subcommand groups land in M0 tasks:
//! M0.3 wires `hook`, M0.6 the orchestrator daemon (`start`), M0.11 the
//! project management commands.

use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use ccteam_core::{CcteamPaths, Orchestrator, OrchestratorConfig};

#[derive(Parser)]
#[command(
    name = "ccteam",
    version,
    about = "Autonomous AI development orchestrator built on Claude Code",
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Hook handlers invoked by Claude Code per project settings.json.
    /// Each subcommand reads stdin JSON (the Claude Code hook payload)
    /// and performs its side effect; stdout is normally empty.
    Hook {
        #[command(subcommand)]
        cmd: HookCommand,
    },
    /// Run the orchestrator daemon. M0 only supports `--foreground`
    /// mode (the flag is accepted for forward compat with M1 daemon
    /// mode but currently a no-op; `start` always runs in foreground).
    Start {
        #[arg(long, default_value_t = false)]
        foreground: bool,
        /// Override the polling tick interval (debug / tests only).
        #[arg(long, value_name = "SECONDS", default_value_t = 30)]
        tick_seconds: u64,
    },
}

#[derive(Subcommand)]
enum HookCommand {
    /// Append one event line to ~/.ccteam/progress/<slug>.jsonl.
    /// `event_type` is the `event` field on the resulting JSONL record
    /// (e.g. "PreToolUse" / "Stop" / "session_start").
    ProgressAppend {
        event_type: String,
    },
    /// Stop hook: parse last assistant message for `PHASE_DONE: <phase>`
    /// or `ESCALATE: <reason>` and emit the matching progress event.
    ParsePhaseEnd,
    /// PostToolUse hook: refresh state.json `context_tokens_used` from
    /// the latest assistant message's `usage.*`. Dollar costs land in
    /// M0.14.
    CostAccumulate,
    /// SessionStart hook: write the `<project>/.ccteam/ready` marker.
    /// M0.10 extends this to bridge a pre-reset progress summary.
    LoadContext,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let Some(command) = cli.command else {
        return Ok(());
    };

    match command {
        Command::Hook { cmd } => run_hook(cmd),
        Command::Start {
            foreground: _,
            tick_seconds,
        } => run_start(tick_seconds),
    }
}

fn run_start(tick_seconds: u64) -> Result<()> {
    init_tracing();
    let paths = CcteamPaths::from_env()?;
    let config = OrchestratorConfig {
        tick_interval: Duration::from_secs(tick_seconds.max(1)),
        ..OrchestratorConfig::default()
    };
    let orchestrator = Orchestrator::new(paths, config)?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")?;
    runtime.block_on(async move {
        let shutdown = async {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("ctrl+c received");
        };
        orchestrator.run(shutdown).await
    })
}

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,ccteam_core=info")),
        )
        .try_init();
}

fn run_hook(cmd: HookCommand) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    let stdin: serde_json::Value = serde_json::from_reader(std::io::stdin().lock())
        .context("parse hook stdin as JSON")?;

    match cmd {
        HookCommand::ProgressAppend { event_type } => {
            ccteam_hooks::progress_append(&paths, &event_type, &stdin)
        }
        HookCommand::ParsePhaseEnd => ccteam_hooks::parse_phase_end(&paths, &stdin),
        HookCommand::CostAccumulate => ccteam_hooks::cost_accumulate(&paths, &stdin),
        HookCommand::LoadContext => ccteam_hooks::load_context(&paths, &stdin),
    }
}
