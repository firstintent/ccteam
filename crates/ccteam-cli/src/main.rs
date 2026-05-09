//! `cct` binary entry point (V0.2.2 F39 renamed from `ccteam`).

mod commands;
mod mcp_serve;
mod team_factory_cli;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use ccteam_core::{CcteamPaths, Orchestrator, OrchestratorConfig};
use commands::{InitOptions, OutputFormat};

#[derive(Parser)]
#[command(
    name = "cct",
    version,
    about = "Autonomous AI development orchestrator built on Claude Code",
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// One-shot setup: create `~/.ccteam/` skeleton, unpack phase
    /// templates to `~/.ccteam/phases/`, and run a quick health check
    /// (claude / tmux / ccteam-on-PATH). Idempotent — safe to re-run.
    Init {
        /// Overwrite existing global phase templates (default: skip if
        /// already on disk so hand-edits stick).
        #[arg(long, default_value_t = false)]
        force: bool,
    },
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
        /// Skip the M0.5.3 phase tools_required check at startup.
        /// Use when an old project on disk has phase YAML referencing
        /// a plugin agent whose plugin isn't installed yet (run
        /// `claude /plugin add <id>` first).
        #[arg(long, default_value_t = false)]
        skip_tool_check: bool,
        /// F29 — override the claude argv used to spawn project tmux
        /// sessions. Whitespace-split. Wins over `CCTEAM_CLAUDE_ARGV`
        /// env which wins over the production default
        /// (`claude --dangerously-skip-permissions --model …`). Used
        /// by e2e harnesses to inject a stub claude (eg
        /// `--claude-argv "sh -c 'cat > /dev/null'"`) so phase
        /// pipeline can be exercised without burning LLM cost.
        #[arg(long, value_name = "ARGV")]
        claude_argv: Option<String>,
    },
    /// Create a new project from a one-line request.
    New {
        /// The request text. Ignored when `--file` is given.
        request: Option<String>,
        /// Read the request from a file instead of the positional arg.
        #[arg(short, long, value_name = "PATH")]
        file: Option<PathBuf>,
        /// Team to run this project under. Default `dev` keeps the
        /// shipped pipeline (plan-eng → implement → … → ship). Other
        /// teams (research, design, ...) land in M3.4.
        #[arg(long, default_value = "dev")]
        team: String,
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
    /// List the cross-project decisions queue — every project's
    /// outbox file with `event_kind: clarify | escalation`. Surfaces
    /// pending user-decision points across all running projects so the
    /// meta-agent can offer "you have N pending decisions, want to
    /// walk through them?" on attach (interfaces.md §5.6.4).
    Decisions {
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
    /// M2.5: run the ccteam MCP server (stdio JSON-RPC). Wired into
    /// `~/.claude.json` `mcpServers.ccteam` by `cct doctor
    /// --install-mcp` so daily-driver claude sessions and the meta
    /// agent both see the 9-tool surface (interfaces §12).
    McpServe,
    /// Stop the running orchestrator daemon. Sends SIGTERM via the
    /// pidfile so the loop drains gracefully. Does **not** kill any
    /// tmux sessions — `cct start` reattaches to them on next launch
    /// (M1.5).
    Stop,
    /// Health checks + tool-surface maintenance.
    Doctor {
        /// Print + return what would happen without touching the
        /// filesystem.
        #[arg(long, default_value_t = false)]
        dry_run: bool,
        /// Overwrite operator hand-edits (memory bridge / shipped team
        /// seeds). Use cautiously.
        #[arg(long, default_value_t = false)]
        force: bool,
        /// Cross-check every shipped phase template's tools_required
        /// against the live tool surface (plugin pipeline + user
        /// agents/skills + MCP servers) and print a markdown report.
        #[arg(long, default_value_t = false)]
        tool_surface: bool,
        /// Install the `cct-control` skill at
        /// `~/.claude/skills/cct-control/SKILL.md` (M1.8). Idempotent.
        #[arg(long, default_value_t = false)]
        install_skill: bool,
        /// Bootstrap a meta-agent project (M1.0) for the given user
        /// handle. Creates `~/projects/<handle>-meta/` with the
        /// dispatcher role prompt + inbox/outbox dirs and (always)
        /// installs the `cct-control` skill. Pass the user handle as
        /// the value (e.g. `--install-meta-agent rob`).
        #[arg(long, value_name = "HANDLE")]
        install_meta_agent: Option<String>,
        /// M2.5: register `mcpServers.ccteam` in `~/.claude.json` so
        /// daily-driver claude + meta-agent both see the ccteam MCP
        /// server (9 tools, interfaces §12). Idempotent — overwrites
        /// any prior `ccteam` server entry but preserves other servers.
        #[arg(long, default_value_t = false)]
        install_mcp: bool,
        /// M4.2: write `~/.claude/rules/ccteam-lessons-<team>.md`
        /// with `<!-- ccteam-managed:lessons begin/end -->` markers + `paths:`
        /// frontmatter scope. Idempotent — re-runs no-op when markers are
        /// intact, repair (single canonical block at end-of-file) when not.
        /// User content outside markers is preserved. Discovers teams by
        /// scanning `~/.ccteam/teams/<name>/team.yaml` for non-empty
        /// `retro_schema` (V0.2 M0.16.2).
        #[arg(long, default_value_t = false)]
        install_memory_bridge: bool,
        /// V0.2 M0.16.2: re-write every shipped team's seed
        /// (`~/.ccteam/teams/<name>/team.yaml` + `~/.ccteam/<phase_dir>/*.md`)
        /// from the in-binary bundle. `force=false` preserves operator
        /// hand-edits; pair with `--force` to clobber. Useful after a
        /// ccteam upgrade ships schema-additive team.yaml fields.
        #[arg(long, default_value_t = false)]
        reset_shipped_teams: bool,
        /// V0.2 M0.18.5: load + validate the named team's
        /// `team.yaml` and every phase markdown under its phase dir.
        /// Fails-loud on schema violations and IO-contract gaps; warns
        /// (without failing) on protocol-literal residue in phase
        /// bodies. Pass the team name as the value (e.g.
        /// `--validate-team dev`).
        #[arg(long, value_name = "TEAM")]
        validate_team: Option<String>,
        /// V0.2 M0.20: remove stale `~/.claude/agents/<name>.md`
        /// symlinks left by the V0.1 `--install-recommended-agents`
        /// path. Spawned project sessions now resolve plugin agents
        /// through Claude Code's in-memory plugin pipeline via
        /// `enabledPlugins` in `<project>/.claude/settings.json`, so
        /// these symlinks are obsolete. Idempotent — no-op when no
        /// marketplace symlinks remain.
        #[arg(long, default_value_t = false)]
        migrate_recommended_agents: bool,
    },
    /// V0.2 M0.18.6: render the orchestrator's per-phase inject
    /// prompt (frontmatter-driven) plus the `@`-referenced phase
    /// markdown body for visual debugging. Pure read-only — does not
    /// touch any session.
    Phase {
        #[command(subcommand)]
        cmd: PhaseCommand,
    },
    /// V0.2 M0.21: translation layer that surfaces project anomalies
    /// (auto-loop cycle / cost / phase duration / daemon down /
    /// needs_attention.outbox) to the meta-agent as NL notifications.
    /// Pure read-only — never mutates orchestrator state, never kills
    /// sessions, never re-injects prompts.
    Watchdog {
        #[command(subcommand)]
        cmd: WatchdogCommand,
    },
    /// V0.2 M0.22: author + publish a cct team as a Claude Code
    /// plugin. `init` scaffolds the staging tree under
    /// `~/.config/ccteam/teams/<name>/`; `publish` links it into the
    /// `ccteam-local` marketplace or pushes it to a GitHub repo.
    Team {
        #[command(subcommand)]
        cmd: TeamCommand,
    },
}

#[derive(Subcommand)]
enum WatchdogCommand {
    /// Scan all projects + daemon heartbeat once and print every alert
    /// that survives `~/.ccteam/watchdog.yaml` filtering. With
    /// `--push --user <handle>` each alert is also appended to the
    /// meta-agent session's outbox.
    Scan {
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
        /// Also write each surviving alert to
        /// `~/projects/<handle>-meta/.ccteam/outbox/`. Pair with
        /// `--user <handle>`.
        #[arg(long, default_value_t = false)]
        push: bool,
        /// User handle whose meta-agent outbox receives pushed alerts.
        /// Required when `--push` is passed.
        #[arg(long, value_name = "HANDLE")]
        user: Option<String>,
    },
}

#[derive(Subcommand)]
enum PhaseCommand {
    /// Render `<team>` `<phase>`'s inject prompt + body. Useful when
    /// authoring phase yamls / debugging unexpected phase behavior.
    Show {
        /// Team name as registered under `~/.ccteam/teams/`.
        team: String,
        /// Phase name (e.g. `implement`, not the prefixed filename).
        phase: String,
    },
}

#[derive(Subcommand)]
enum TeamCommand {
    /// Scaffold a new team plugin staging tree at
    /// `~/.config/ccteam/teams/<name>/`. Creates plugin.json + team.yaml
    /// + a starter phase + README. Idempotent.
    Init {
        /// Team / plugin name (ascii lower / digit / `-` / `_`).
        name: String,
        /// One-line plugin description.
        #[arg(long, default_value = "")]
        description: String,
        /// Plugin manifest `author.name`. Required.
        #[arg(long)]
        author_name: String,
        /// Plugin manifest `author.email` (optional).
        #[arg(long)]
        author_email: Option<String>,
        /// Plugin manifest `version` (optional, eg `0.1.0`).
        #[arg(long)]
        version: Option<String>,
    },
    /// Publish a staged team. `--target local` symlinks staging into
    /// `~/.claude/plugins/marketplaces/ccteam-local/plugins/<name>/`;
    /// `--target github --repo <owner>/<name>` runs `gh repo create`
    /// + push. Validates the staging tree before any side-effect.
    Publish {
        name: String,
        #[arg(long, value_enum, default_value_t = team_factory_cli::PublishTargetArg::Local)]
        target: team_factory_cli::PublishTargetArg,
        /// `<owner>/<name>` repo coordinate (required for `--target github`).
        #[arg(long)]
        repo: Option<String>,
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
    /// V0.2 M0.19.3 PreToolUse hook for `AskUserQuestion`. Returns a
    /// `permissionDecision: deny` so the assistant routes through the
    /// outbox / clarify protocol instead of synchronously waiting on
    /// an offline user.
    InterceptAsk,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let Some(command) = cli.command else {
        return Ok(());
    };

    match command {
        Command::Init { force } => {
            let paths = CcteamPaths::from_env()?;
            let report = commands::run_init(&paths, InitOptions { force })?;
            print!("{report}");
            Ok(())
        }
        Command::Hook { cmd } => run_hook(cmd),
        Command::Start {
            foreground: _,
            tick_seconds,
            skip_tool_check,
            claude_argv,
        } => run_start(tick_seconds, skip_tool_check, claude_argv),
        Command::New { request, file, team } => run_new(request, file, team),
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
        Command::Decisions { format } => {
            let paths = CcteamPaths::from_env()?;
            let body = commands::run_decisions(&paths, format)?;
            print!("{body}");
            Ok(())
        }
        Command::McpServe => {
            init_tracing();
            let paths = CcteamPaths::from_env()?;
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("build tokio runtime for mcp-serve")?;
            runtime.block_on(mcp_serve::run_mcp_serve(paths))
        }
        Command::Stop => run_stop(),
        Command::Doctor {
            dry_run,
            force,
            tool_surface,
            install_skill,
            install_meta_agent,
            install_mcp,
            install_memory_bridge,
            reset_shipped_teams,
            validate_team,
            migrate_recommended_agents,
        } => run_doctor(commands::DoctorOptions {
            dry_run,
            force,
            tool_surface,
            install_skill,
            install_meta_agent,
            install_mcp,
            install_memory_bridge,
            reset_shipped_teams,
            validate_team,
            migrate_recommended_agents,
        }),
        Command::Phase { cmd } => run_phase(cmd),
        Command::Watchdog { cmd } => run_watchdog(cmd),
        Command::Team { cmd } => run_team(cmd),
    }
}

fn run_watchdog(cmd: WatchdogCommand) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    match cmd {
        WatchdogCommand::Scan { format, push, user } => {
            if push && user.is_none() {
                anyhow::bail!("`--push` requires `--user <handle>`");
            }
            let handle = if push { user.as_deref() } else { None };
            let body = commands::run_watchdog_scan(&paths, format, handle)?;
            print!("{body}");
            Ok(())
        }
    }
}

fn run_team(cmd: TeamCommand) -> Result<()> {
    match cmd {
        TeamCommand::Init {
            name,
            description,
            author_name,
            author_email,
            version,
        } => {
            let body = team_factory_cli::run_team_init(&team_factory_cli::TeamInitArgs {
                name,
                description,
                author_name,
                author_email,
                version,
            })?;
            print!("{body}");
            Ok(())
        }
        TeamCommand::Publish { name, target, repo } => {
            let body = team_factory_cli::run_team_publish(&team_factory_cli::TeamPublishArgs {
                name,
                target,
                repo,
            })?;
            print!("{body}");
            Ok(())
        }
    }
}

fn run_phase(cmd: PhaseCommand) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    match cmd {
        PhaseCommand::Show { team, phase } => {
            let body = commands::run_phase_show(&paths, &team, &phase)?;
            print!("{body}");
            Ok(())
        }
    }
}

fn run_stop() -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    match ccteam_core::send_sigterm_to_pidfile(&paths)? {
        Some(pid) => {
            println!("cct stop: SIGTERM sent to orchestrator pid {pid}");
            println!(
                "tmux sessions are NOT killed — `cct start` will reattach to them.",
            );
            Ok(())
        }
        None => {
            println!("cct stop: no running orchestrator (pidfile absent or stale).");
            Ok(())
        }
    }
}

fn run_doctor(opts: commands::DoctorOptions) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    let body = commands::run_doctor(&paths, opts)?;
    print!("{body}");
    Ok(())
}

fn run_hook(cmd: HookCommand) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    let stdin: serde_json::Value = serde_json::from_reader(std::io::stdin().lock())
        .context("parse hook stdin as JSON")?;

    match cmd {
        HookCommand::ProgressAppend { event_type } => {
            ccteam_hooks::progress_append(&paths, &event_type, &stdin)
        }
        HookCommand::ParsePhaseEnd => {
            let decision = ccteam_hooks::parse_phase_end(&paths, &stdin)?;
            match decision {
                ccteam_hooks::ParseDecision::Continue => Ok(()),
                ccteam_hooks::ParseDecision::Block { reason } => {
                    let json = serde_json::json!({
                        "decision": "block",
                        "reason": reason,
                    });
                    println!("{}", serde_json::to_string(&json)?);
                    Ok(())
                }
                ccteam_hooks::ParseDecision::BlockMissingOutput { stderr } => {
                    // V0.2 M0.19: exit 2 + stderr is the Stop hook
                    // contract Claude Code interprets as a blocking
                    // system message (`hooks.ts:2784-2805`). The
                    // assistant is forced to re-prompt with the
                    // stderr text injected.
                    eprintln!("{stderr}");
                    std::process::exit(2);
                }
            }
        }
        HookCommand::CostAccumulate => ccteam_hooks::cost_accumulate(&paths, &stdin),
        HookCommand::LoadContext => ccteam_hooks::load_context(&paths, &stdin),
        HookCommand::InterceptAsk => {
            let decision = ccteam_hooks::intercept_ask_decision();
            println!("{}", serde_json::to_string(&decision)?);
            Ok(())
        }
    }
}

fn run_start(
    tick_seconds: u64,
    skip_tool_check: bool,
    claude_argv_flag: Option<String>,
) -> Result<()> {
    init_tracing();
    let paths = CcteamPaths::from_env()?;

    // V0.2 M0.16.2: self-heal shipped team seeds on every daemon
    // start. force=false skips existing files so operator hand-edits
    // are preserved; the call still ensures `~/.ccteam/teams/<name>/team.yaml`
    // exists for every shipped team (dev / product-research /
    // meta-agent) — without this, a fresh install missing `cct init`
    // would leave `is_evergreen("meta-agent") == false` and the
    // dispatcher would fall into the phase-DAG path.
    if let Err(err) = ccteam_core::write_all_global_team_templates(&paths.root, false) {
        tracing::warn!(
            error = %err,
            root = %paths.root.display(),
            "cct start: could not seed shipped team templates; \
             run `cct doctor --reset-shipped-teams` if teams are missing",
        );
    }

    if !paths.phases_dir().exists() {
        eprintln!(
            "ccteam: phases dir {} not found.\n  run `cct init` once to unpack templates, then come back.",
            paths.phases_dir().display()
        );
    }
    if !paths.projects_root.exists()
        || std::fs::read_dir(&paths.projects_root)
            .map(|mut it| it.next().is_none())
            .unwrap_or(true)
    {
        eprintln!(
            "ccteam: no projects under {} yet.\n  start one in another terminal: cct new \"<your idea>\"",
            paths.projects_root.display()
        );
    }

    // F29 — precedence: CLI flag > CCTEAM_CLAUDE_ARGV env > production
    // default. `OrchestratorConfig::default()` already reads the env;
    // the flag layers on top.
    let mut config = OrchestratorConfig {
        tick_interval: Duration::from_secs(tick_seconds.max(1)),
        skip_tool_check,
        ..OrchestratorConfig::default()
    };
    if let Some(raw) = claude_argv_flag {
        let parts: Vec<String> = raw.split_whitespace().map(String::from).collect();
        if !parts.is_empty() {
            config.claude_argv = parts;
        }
    }
    // Write the pidfile *before* constructing the orchestrator so a
    // second `cct start` against the same root errors out cleanly
    // before either side has touched tmux.
    let pidfile = ccteam_core::write_pidfile(&paths)?;
    tracing::info!(pidfile = %pidfile.display(), "orchestrator pidfile written");

    // We need the paths twice (for orchestrator construction + final
    // pidfile cleanup), so clone before the move into Orchestrator::new.
    let cleanup_paths = paths.clone();
    let result = (|| -> Result<()> {
        let orchestrator = Orchestrator::new(paths, config)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("build tokio runtime")?;
        runtime.block_on(async move {
            let shutdown = async {
                #[cfg(unix)]
                {
                    use tokio::signal::unix::{signal, SignalKind};
                    let mut sigterm = match signal(SignalKind::terminate()) {
                        Ok(s) => s,
                        Err(err) => {
                            tracing::warn!(?err, "could not install SIGTERM handler");
                            let _ = tokio::signal::ctrl_c().await;
                            tracing::info!("ctrl+c received");
                            return;
                        }
                    };
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => tracing::info!("ctrl+c received"),
                        _ = sigterm.recv() => tracing::info!("SIGTERM received (cct stop)"),
                    }
                }
                #[cfg(not(unix))]
                {
                    let _ = tokio::signal::ctrl_c().await;
                    tracing::info!("ctrl+c received");
                }
            };
            orchestrator.run(shutdown).await
        })
    })();
    ccteam_core::remove_pidfile(&cleanup_paths);
    result
}

fn run_new(request: Option<String>, file: Option<PathBuf>, team: String) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    let body = match (file, request) {
        (Some(path), _) => std::fs::read_to_string(&path)
            .with_context(|| format!("read {}", path.display()))?,
        (None, Some(text)) => text,
        (None, None) => {
            anyhow::bail!("cct new: provide a request as a positional arg or --file PATH")
        }
    };
    let slug = commands::run_new(&paths, body.trim(), &team)?;
    println!("created project {slug} (team: {team})");
    println!("  spec   : {}", paths.project_ccteam_dir(&slug).join("spec.md").display());
    println!("  state  : {}", paths.project_state(&slug).display());
    println!("  config : {}", paths.project_dir(&slug).join(".claude/settings.json").display());
    println!("\nrun `cct start --foreground` (in another terminal) to dispatch the first phase.");
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
