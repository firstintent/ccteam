//! `ccteam` binary entry point.

mod commands;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use ccteam_core::{CcteamPaths, Orchestrator, OrchestratorConfig};
use commands::OutputFormat;

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
    /// Create a new project from a one-line request.
    New {
        /// The request text. Ignored when `--file` is given.
        request: Option<String>,
        /// Read the request from a file instead of the positional arg.
        #[arg(short, long, value_name = "PATH")]
        file: Option<PathBuf>,
    },
    /// List all known projects.
    Ls {
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
    /// Show one project's full state, recent events, and artifacts.
    Show {
        slug: String,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
    /// Attach to a project's tmux session (`tmux attach`).
    Attach { slug: String },
    /// Capture the project's pane content without attaching.
    Peek { slug: String },
    /// Print the project's progress.jsonl, optionally tailing.
    Progress {
        slug: String,
        #[arg(long)]
        tail: bool,
    },
    /// Resume a paused / escalated project (re-arm phase_state=idle).
    Resume { slug: String },
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
        Command::New { request, file } => run_new(request, file),
        Command::Ls { format } => run_ls(format),
        Command::Show { slug, format } => run_show(&slug, format),
        Command::Attach { slug } => commands::run_attach(&slug),
        Command::Peek { slug } => run_peek(&slug),
        Command::Progress { slug, tail } => {
            let paths = CcteamPaths::from_env()?;
            commands::run_progress(&paths, &slug, tail)
        }
        Command::Resume { slug } => {
            let paths = CcteamPaths::from_env()?;
            commands::run_resume(&paths, &slug)
        }
    }
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

fn run_new(request: Option<String>, file: Option<PathBuf>) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    let body = match (file, request) {
        (Some(path), _) => std::fs::read_to_string(&path)
            .with_context(|| format!("read {}", path.display()))?,
        (None, Some(text)) => text,
        (None, None) => {
            anyhow::bail!("ccteam new: provide a request as a positional arg or --file PATH")
        }
    };
    let slug = commands::run_new(&paths, body.trim())?;
    println!("created project {slug}");
    println!("  spec   : {}", paths.project_ccteam_dir(&slug).join("spec.md").display());
    println!("  state  : {}", paths.project_state(&slug).display());
    println!("  config : {}", paths.project_dir(&slug).join(".claude/settings.json").display());
    println!("\nrun `ccteam start --foreground` (in another terminal) to dispatch the first phase.");
    Ok(())
}

fn run_ls(format: OutputFormat) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    let body = commands::run_ls(&paths, format)?;
    print!("{body}");
    Ok(())
}

fn run_show(slug: &str, format: OutputFormat) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    let body = commands::run_show(&paths, slug, format)?;
    print!("{body}");
    Ok(())
}

fn run_peek(slug: &str) -> Result<()> {
    let body = commands::run_peek(slug)?;
    print!("{body}");
    Ok(())
}

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,ccteam_core=info")),
        )
        .try_init();
}
