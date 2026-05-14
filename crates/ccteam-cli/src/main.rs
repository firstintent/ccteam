//! `ccteam` binary entry point.

mod commands;
mod mcp_serve;
// V0.4.0 F65 — meta-agent MCP workflow tools (7 new). Lives in its own
// module to keep `mcp_serve.rs` focused on the M2.5 protocol surface
// while the workflow tools accumulate in lockstep with the F66
// orchestrator.
mod mcp_workflow_tools;
mod team_factory_cli;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use ccteam_core::{CcteamPaths, Orchestrator, OrchestratorConfig};
use commands::{InitOptions, OutputFormat};

#[derive(Parser)]
#[command(
    name = "ccteam",
    version,
    about = "Autonomous AI development orchestrator built on Claude Code"
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
    /// Run the orchestrator daemon (and, by default, the web UI on
    /// `127.0.0.1:7331` in the same process). Foreground is the only
    /// supported mode — `ccteam start` is enough; the `--foreground`
    /// flag is accepted for back-compat but no longer required.
    /// Pass `--no-web` to run orchestrator only.
    Start {
        /// Back-compat no-op: foreground is the only mode.
        #[arg(long, default_value_t = false, hide = true)]
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
        /// Skip starting the embedded web UI. Use this when you want
        /// orchestrator only (e.g. headless server, custom web bind
        /// via a separate `ccteam web` invocation).
        #[arg(long, default_value_t = false)]
        no_web: bool,
        /// Embedded web UI bind address. Loopback (default) disables
        /// auth; non-loopback generates `~/.ccteam/web-token` and
        /// requires `Authorization: Bearer ccteam:<token>`.
        #[arg(long, value_name = "ADDR", default_value = "127.0.0.1:7331")]
        web_bind: String,
        /// Disable token auth on the embedded web. DANGEROUS on
        /// non-loopback bind — prints a 5-second warning before
        /// listening.
        #[arg(long, default_value_t = false)]
        web_no_auth: bool,
        /// Custom path to read the auth token from (default
        /// `~/.ccteam/web-token`).
        #[arg(long, value_name = "PATH")]
        web_token_file: Option<PathBuf>,
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
        /// V0.2.2 F34 Tier 1 — explicit slug override. When set, skips
        /// the Tier 3 `claude -p` smart-suggest and Tier 4 deterministic
        /// fallback. Validated `[a-z0-9-]+`, len ≤ 60. B2 prefix
        /// semantics: if the value already starts with `<team>-` it's
        /// kept verbatim, otherwise `<team>-` is prepended automatically
        /// (PRD §3.2.1).
        #[arg(long, value_name = "NAME")]
        slug: Option<String>,
        /// V0.2.2 F34 Tier 3 — when set, skip the `claude -p` smart-
        /// suggest path and fall back directly to the deterministic
        /// `slugify_brief()` Tier 4. Useful in scripts / CI where you
        /// don't want the per-invocation LLM round trip.
        #[arg(long, default_value_t = false)]
        no_auto_slug: bool,
        /// V0.2.2 F34 Tier 3 — model name passed to `claude -p` for
        /// the smart-suggest fallback. Default `claude-haiku-4-5-20251001`
        /// (cheapest + fastest viable). Override with eg
        /// `claude-sonnet-4-5-20251001` for harder briefs.
        #[arg(
            long,
            value_name = "MODEL",
            default_value = "claude-haiku-4-5-20251001"
        )]
        auto_slug_model: String,
    },
    /// List all known projects.
    Ls {
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
    /// Show one project's full state, recent events, and artifacts.
    /// With no slug, lists every available slug + a re-run hint.
    Show {
        slug: Option<String>,
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
    /// Send a free-form message to a project's inbox. Wraps the
    /// MCP-equivalent `send_to_session` so users don't have to
    /// hand-write the markdown frontmatter. Orchestrator auto-routes
    /// the message to a worker (or the meta-agent in V0.4.1+) on
    /// the next tick.
    Send {
        /// Project slug (or meta-agent handle for meta-agent inbox).
        slug: String,
        /// Optional target role override (writes frontmatter
        /// `target_role: <role>`). Otherwise routing follows the
        /// inbox default rules.
        #[arg(short = 'r', long)]
        role: Option<String>,
        /// If set, frontmatter `no_spawn: true` is added so the
        /// message is archived for audit only (no auto-spawn).
        #[arg(long, default_value_t = false)]
        no_spawn: bool,
        /// Message body. Use `-` to read from stdin.
        body: String,
    },
    /// Trigger a fresh spawn of `<role>` in `<slug>` with an optional
    /// kick prompt. Writes a `.ccteam/spawn_requests/<role>-<ts>.json`
    /// marker the orchestrator picks up on its next tick. CLI shortcut
    /// for the MCP `ccteam__spawn_agent` tool.
    Spawn {
        /// Project slug.
        slug: String,
        /// Workflow role (must exist in `<project>/workflow.yaml`).
        role: String,
        /// Optional initial prompt. Falls back to the default kick
        /// prompt when omitted (let the role's `.claude/agents/<role>.md`
        /// drive). Use `-` to read from stdin.
        prompt: Option<String>,
    },
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
    /// `~/.claude.json` `mcpServers.ccteam` by `ccteam doctor
    /// --install-mcp` so daily-driver claude sessions and the meta
    /// agent both see the 9-tool surface (interfaces §12).
    McpServe,
    /// Stop the running orchestrator daemon. Sends SIGTERM via the
    /// pidfile so the loop drains gracefully. Does **not** kill any
    /// tmux sessions — `ccteam start` reattaches to them on next launch
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
        /// Install the `ccteam-control` skill at
        /// `~/.claude/skills/ccteam-control/SKILL.md` (M1.8). Idempotent.
        #[arg(long, default_value_t = false)]
        install_skill: bool,
        /// Bootstrap a meta-agent project (M1.0) for the given user
        /// handle. Creates `~/projects/meta-<handle>/` with the
        /// dispatcher role prompt + inbox/outbox dirs and (always)
        /// installs the `ccteam-control` skill. Pass the user handle as
        /// the value (e.g. `--install-meta-agent rob`).
        #[arg(long, value_name = "HANDLE")]
        install_meta_agent: Option<String>,
        /// M2.5: register `mcpServers.ccteam` in `~/.claude.json` so
        /// daily-driver claude + meta-agent both see the ccteam MCP
        /// server (9 tools, interfaces §12). Idempotent — overwrites
        /// any prior `ccteam` server entry but preserves other servers.
        #[arg(long, default_value_t = false)]
        install_mcp: bool,
        /// V0.4.1: aggregate first-run setup. Equivalent to
        /// `--install-mcp --install-skill --install-meta-agent <HANDLE>`.
        /// Pass the user handle as the value
        /// (e.g. `--install-all rob`). Idempotent.
        #[arg(long, value_name = "HANDLE")]
        install_all: Option<String>,
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
        /// V0.2.2 F38: render a one-shot PNG screenshot of the given
        /// project's tmux pane to verify the `vt100 + imageproc` path
        /// end-to-end. Reports the font in use + the resulting PNG
        /// path or the degrade reason (tmux missing / font / IO).
        #[arg(long, value_name = "SLUG")]
        screenshot_smoke: Option<String>,
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
    /// V0.2 M0.22: author + publish a ccteam team as a Claude Code
    /// plugin. `init` scaffolds the staging tree under
    /// `~/.config/ccteam/teams/<name>/`; `publish` links it into the
    /// `ccteam-local` marketplace or pushes it to a GitHub repo.
    Team {
        #[command(subcommand)]
        cmd: TeamCommand,
    },
    /// V0.3.1 F49 — adhoc multi-session primitives for `kind: flex`
    /// teams. `add --harness claude` creates a registered tmux session;
    /// `add --harness codex` still returns the V0.3.2 stub error.
    Session {
        #[command(subcommand)]
        action: SessionAction,
    },
    /// V0.3 M5.0: serve the ccteam web UI (read + restricted-write,
    /// `docs/v0-3/prd.md` §3-§6). M5.0 ships the scaffold + `/health`
    /// endpoint only; dashboard / SSE / write actions land in M5.1-3.
    Web {
        /// Listen address. Default `127.0.0.1:7331` — auth is disabled
        /// when bound to loopback. Bind to `0.0.0.0:<port>` for LAN
        /// reach (M5.3 requires token auth on non-loopback unless
        /// `--no-auth`).
        #[arg(long, default_value = "127.0.0.1:7331")]
        bind: String,
        /// Disable token auth on write endpoints. DANGEROUS on
        /// non-loopback bind — M5.3 prints a 5-second warning before
        /// listening.
        #[arg(long, default_value_t = false)]
        no_auth: bool,
        /// Custom path to read the auth token from (default
        /// `~/.ccteam/web-token`). M5.3 consumes this; M5.0 records
        /// it on `ServeOpts` for shape stability.
        #[arg(long, value_name = "PATH")]
        token_file: Option<PathBuf>,
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
        /// `~/projects/meta-<handle>/.ccteam/outbox/`. Pair with
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
        /// Team execution kind. `flex` skips phase scaffolding.
        #[arg(long, value_enum, default_value_t = team_factory_cli::TeamKindArg::Workflow)]
        kind: team_factory_cli::TeamKindArg,
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

/// V0.3.1 F49 — `ccteam session` subcommand surface for flex teams.
#[derive(Subcommand)]
enum SessionAction {
    /// Add a new harness session to an existing flex project.
    Add {
        /// Project slug (must already exist under `~/projects/`).
        slug: String,
        /// Harness backing the new session. Defaults to `claude` to
        /// match `HarnessKind::default()`.
        #[arg(long, value_enum, default_value_t = HarnessKindCli::Claude)]
        harness: HarnessKindCli,
        /// V0.4.0 F61 — agent role for `claude --bg --agent <role>`.
        /// Resolves against the project's `.claude/agents/<role>.md`.
        /// Required for claude sessions; ignored for codex (codex
        /// threads role-equivalents through its own argv).
        #[arg(long, default_value = "main")]
        role: String,
    },
    /// List sessions registered for a flex project.
    Ls {
        /// Project slug.
        slug: String,
    },
    /// Attach to one specific session of a flex project.
    Attach {
        /// Project slug.
        slug: String,
        /// Session id (e.g. `claude-1`, `codex-2`).
        sid: String,
    },
    /// Remove (graceful shutdown) one session of a flex project.
    Rm {
        /// Project slug.
        slug: String,
        /// Session id to remove.
        sid: String,
    },
}

/// V0.3.1 F47 — CLI surface mirror of [`ccteam_core::HarnessKind`].
/// Lives in the CLI crate so `clap::ValueEnum` derivation doesn't
/// pollute the core schema enum (which deserves to round-trip yaml /
/// json without clap dependencies). Convert via `match` at dispatch
/// time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum HarnessKindCli {
    Claude,
    Codex,
}

#[derive(Subcommand)]
enum HookCommand {
    /// Append one event line to ~/.ccteam/progress/<slug>.jsonl.
    /// `event_type` is the `event` field on the resulting JSONL record
    /// (e.g. "PreToolUse" / "Stop" / "session_start").
    ProgressAppend { event_type: String },
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

    // No subcommand → print help instead of silently exiting. Prior
    // behavior (V0.4.0 and earlier) was Ok(()) with nothing on stdout,
    // which left users wondering whether ccteam was installed at all.
    let Some(command) = cli.command else {
        use clap::CommandFactory;
        Cli::command().print_help().context("print help")?;
        println!();
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
            no_web,
            web_bind,
            web_no_auth,
            web_token_file,
        } => run_start(
            tick_seconds,
            skip_tool_check,
            claude_argv,
            StartWebOpts {
                disabled: no_web,
                bind: web_bind,
                no_auth: web_no_auth,
                token_file: web_token_file,
            },
        ),
        Command::New {
            request,
            file,
            team,
            slug,
            no_auto_slug,
            auto_slug_model,
        } => run_new(request, file, team, slug, no_auto_slug, auto_slug_model),
        Command::Ls { format } => run_ls(format),
        Command::Show { slug, format } => match slug {
            Some(s) => run_show(&s, format),
            None => show_slug_picker(),
        },
        Command::Attach { slug } => {
            let paths = CcteamPaths::from_env()?;
            commands::run_attach(&paths, &slug)
        }
        Command::Peek { slug } => run_peek(&slug),
        Command::Progress { slug, tail } => {
            let paths = CcteamPaths::from_env()?;
            commands::run_progress(&paths, &slug, tail)
        }
        Command::Resume { slug } => {
            let paths = CcteamPaths::from_env()?;
            commands::run_resume(&paths, &slug)
        }
        Command::Send {
            slug,
            role,
            no_spawn,
            body,
        } => {
            let paths = CcteamPaths::from_env()?;
            run_send(&paths, &slug, role.as_deref(), no_spawn, &body)
        }
        Command::Spawn {
            slug,
            role,
            prompt,
        } => {
            let paths = CcteamPaths::from_env()?;
            run_spawn(&paths, &slug, &role, prompt.as_deref())
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
            install_all,
            install_memory_bridge,
            reset_shipped_teams,
            validate_team,
            migrate_recommended_agents,
            screenshot_smoke,
        } => {
            // V0.4.1 `--install-all <handle>` is sugar for the three
            // first-run flags. Explicit flags still win where present;
            // we only OR-in the aggregate's components.
            let (final_mcp, final_skill, final_meta) = match install_all {
                Some(h) => (true, true, Some(h)),
                None => (install_mcp, install_skill, install_meta_agent),
            };
            run_doctor(commands::DoctorOptions {
                dry_run,
                force,
                tool_surface,
                install_skill: final_skill,
                install_meta_agent: final_meta,
                install_mcp: final_mcp,
                install_memory_bridge,
                reset_shipped_teams,
                validate_team,
                migrate_recommended_agents,
                screenshot_smoke,
            })
        }
        Command::Phase { cmd } => run_phase(cmd),
        Command::Watchdog { cmd } => run_watchdog(cmd),
        Command::Team { cmd } => run_team(cmd),
        Command::Session { action } => run_session(action),
        Command::Web {
            bind,
            no_auth,
            token_file,
        } => {
            init_tracing();
            commands::run_web(commands::WebOptions {
                bind,
                no_auth,
                token_file,
            })
        }
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
            kind,
        } => {
            let body = team_factory_cli::run_team_init(&team_factory_cli::TeamInitArgs {
                name,
                description,
                author_name,
                author_email,
                version,
                kind,
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

/// V0.3.1 F49 — dispatch `ccteam session <action>` to the flex
/// multi-session handlers.
fn run_session(action: SessionAction) -> Result<()> {
    match action {
        SessionAction::Add {
            slug,
            harness,
            role,
        } => {
            let kind = match harness {
                HarnessKindCli::Claude => ccteam_core::HarnessKind::Claude,
                HarnessKindCli::Codex => ccteam_core::HarnessKind::Codex,
            };
            commands::run_session_add(&slug, kind, role)
        }
        SessionAction::Ls { slug } => commands::run_session_ls(&slug),
        SessionAction::Attach { slug, sid } => commands::run_session_attach(&slug, &sid),
        SessionAction::Rm { slug, sid } => commands::run_session_rm(&slug, &sid),
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
            println!("ccteam stop: SIGTERM sent to orchestrator pid {pid}");
            println!("tmux sessions are NOT killed — `ccteam start` will reattach to them.",);
            Ok(())
        }
        None => {
            println!("ccteam stop: no running orchestrator (pidfile absent or stale).");
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

    match cmd {
        HookCommand::ProgressAppend { event_type } => {
            let stdin = parse_hook_stdin_json()?;
            ccteam_hooks::progress_append(&paths, &event_type, &stdin)
        }
        HookCommand::ParsePhaseEnd => {
            let stdin = parse_hook_stdin_json()?;
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
        HookCommand::CostAccumulate => {
            let stdin = parse_hook_stdin_json()?;
            ccteam_hooks::cost_accumulate(&paths, &stdin)
        }
        HookCommand::LoadContext => {
            let stdin = parse_hook_stdin_json()?;
            ccteam_hooks::load_context(&paths, &stdin)
        }
        HookCommand::InterceptAsk => {
            let decision = ccteam_hooks::intercept_ask_decision();
            println!("{}", serde_json::to_string(&decision)?);
            Ok(())
        }
    }
}

fn parse_hook_stdin_json() -> Result<serde_json::Value> {
    serde_json::from_reader(std::io::stdin().lock()).context("parse hook stdin as JSON")
}

struct StartWebOpts {
    disabled: bool,
    bind: String,
    no_auth: bool,
    token_file: Option<PathBuf>,
}

fn run_start(
    tick_seconds: u64,
    skip_tool_check: bool,
    claude_argv_flag: Option<String>,
    web: StartWebOpts,
) -> Result<()> {
    init_tracing();
    let paths = CcteamPaths::from_env()?;

    // V0.4.0 F60: the shipped team seed writer was deleted with the
    // phase machinery (F63 will reintroduce a workflow seed). Daemon
    // start no longer self-heals — operators supply their own
    // `~/.ccteam/teams/<name>/team.yaml`.

    if !paths.phases_dir().exists() {
        eprintln!(
            "ccteam: phases dir {} not found.\n  run `ccteam init` once to unpack templates, then come back.",
            paths.phases_dir().display()
        );
    }
    if !paths.projects_root.exists()
        || std::fs::read_dir(&paths.projects_root)
            .map(|mut it| it.next().is_none())
            .unwrap_or(true)
    {
        eprintln!(
            "ccteam: no projects under {} yet.\n  start one in another terminal: ccteam new \"<your idea>\"",
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
    // second `ccteam start` against the same root errors out cleanly
    // before either side has touched tmux.
    let pidfile = ccteam_core::write_pidfile(&paths)?;
    tracing::info!(pidfile = %pidfile.display(), "orchestrator pidfile written");

    // Print a single banner up front so the operator can paste the web
    // URL into a browser without grepping mid-log noise. Skip when
    // web is disabled. Token is resolved here (read or pre-generate)
    // only when bind is non-loopback so the loopback fast-path stays
    // zero-IO.
    if !web.disabled {
        print_web_banner(&paths, &web);
    }

    // We need the paths twice (for orchestrator construction + final
    // pidfile cleanup), so clone before the move into Orchestrator::new.
    let cleanup_paths = paths.clone();
    let result = (|| -> Result<()> {
        let orchestrator = std::sync::Arc::new(Orchestrator::new(paths, config)?);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("build tokio runtime")?;
        runtime.block_on(async move {
            // V0.4.1 simplification: orchestrator + web share one
            // shutdown signal (Ctrl-C or SIGTERM). The watch::channel
            // lets multiple awaiters subscribe to the same termination
            // event without consuming a single oneshot.
            let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

            let signal_task = tokio::spawn(async move {
                wait_for_shutdown_signal().await;
                let _ = shutdown_tx.send(true);
            });

            let web_handle = if web.disabled {
                None
            } else {
                let opts = match parse_web_opts(&web) {
                    Ok(o) => o,
                    Err(err) => {
                        signal_task.abort();
                        return Err(err);
                    }
                };
                let mut rx = shutdown_rx.clone();
                Some(tokio::spawn(async move {
                    ccteam_web::serve_with_shutdown(opts, async move {
                        let _ = rx.changed().await;
                    })
                    .await
                }))
            };

            let orch_shutdown = {
                let mut rx = shutdown_rx.clone();
                async move {
                    let _ = rx.changed().await;
                }
            };
            let orch_result = orchestrator.run(orch_shutdown).await;

            // Drain the web task once shutdown propagates.
            if let Some(h) = web_handle {
                match h.await {
                    Ok(Ok(())) => {}
                    Ok(Err(err)) => tracing::warn!(?err, "ccteam web exited with error"),
                    Err(je) if je.is_cancelled() => {}
                    Err(je) => tracing::warn!(?je, "ccteam web task panicked"),
                }
            }
            signal_task.abort();
            orch_result
        })
    })();
    ccteam_core::remove_pidfile(&cleanup_paths);
    result
}

async fn wait_for_shutdown_signal() {
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
            _ = sigterm.recv() => tracing::info!("SIGTERM received (ccteam stop)"),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("ctrl+c received");
    }
}

fn print_web_banner(paths: &CcteamPaths, web: &StartWebOpts) {
    let bind: std::net::SocketAddr = match web.bind.parse() {
        Ok(a) => a,
        Err(_) => {
            // Defer the error to parse_web_opts inside the async block;
            // banner is best-effort.
            return;
        }
    };
    let loopback = ccteam_web::auth::is_loopback(&bind);
    let scheme = "http";
    eprintln!();
    eprintln!("┌─ ccteam web ─────────────────────────────────────────────");
    if loopback || web.no_auth {
        eprintln!("│  URL:   {scheme}://{bind}/");
        if !loopback && web.no_auth {
            eprintln!("│  AUTH:  DISABLED (--web-no-auth on non-loopback — LAN-wide!)");
        } else {
            eprintln!("│  AUTH:  disabled (loopback bind)");
        }
    } else {
        let token_path = web
            .token_file
            .clone()
            .unwrap_or_else(|| ccteam_web::token::default_token_path(paths));
        match ccteam_web::token::generate_or_load_token(&token_path) {
            Ok(hex) => {
                eprintln!("│  URL:   {scheme}://{bind}/?token=ccteam:{hex}");
                eprintln!("│  TOKEN: ccteam:{hex}");
                eprintln!("│  FILE:  {}", token_path.display());
            }
            Err(err) => {
                eprintln!("│  URL:   {scheme}://{bind}/  (token init failed: {err})");
            }
        }
    }
    eprintln!("└──────────────────────────────────────────────────────────");
    eprintln!();
}

fn parse_web_opts(web: &StartWebOpts) -> Result<ccteam_web::ServeOpts> {
    let bind: std::net::SocketAddr = web
        .bind
        .parse()
        .with_context(|| format!("--web-bind {} is not a valid socket address", web.bind))?;
    Ok(ccteam_web::ServeOpts {
        bind,
        no_auth: web.no_auth,
        token_file: web.token_file.clone(),
        no_auth_grace_secs: Some(5),
    })
}

fn show_slug_picker() -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    let projects = ccteam_core::queries::collect_projects(&paths).unwrap_or_default();
    if projects.is_empty() {
        println!("no projects yet — `ccteam new \"<your idea>\"` to make one.");
        return Ok(());
    }
    println!("`ccteam show` needs a slug. Available:");
    for p in &projects {
        println!("  {}", p.state.slug);
    }
    println!();
    println!("re-run as `ccteam show <slug>`.");
    Ok(())
}

fn read_body_or_stdin(body: &str) -> Result<String> {
    if body == "-" {
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin().lock(), &mut buf)
            .context("read body from stdin")?;
        Ok(buf)
    } else {
        Ok(body.to_string())
    }
}

fn run_send(
    paths: &CcteamPaths,
    slug: &str,
    role: Option<&str>,
    no_spawn: bool,
    body: &str,
) -> Result<()> {
    let project_dir = paths.project_dir(slug);
    if !project_dir.exists() {
        anyhow::bail!(
            "no project `{slug}` at {}",
            project_dir.display()
        );
    }
    let body = read_body_or_stdin(body)?;
    let body = body.trim();
    if body.is_empty() {
        anyhow::bail!("ccteam send: refusing to write empty body");
    }
    // Compose a full inbox message frontmatter + body. The orchestrator's
    // check_inbox routes off `target_role` / `no_spawn` so we surface
    // those as CLI flags.
    let now = chrono::Utc::now();
    let ts = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut frontmatter = format!(
        "---\nschema_version: 1\nsource: ccteam-cli\nsource_user: {user}\n\
         created_at: {ts}\ningested_at: {ts}\ncontent_type: text\n",
        user = std::env::var("USER").unwrap_or_else(|_| "ccteam".into()),
        ts = ts,
    );
    if let Some(r) = role {
        frontmatter.push_str(&format!("target_role: {r}\n"));
    }
    if no_spawn {
        frontmatter.push_str("no_spawn: true\n");
    }
    frontmatter.push_str("---\n\n");
    frontmatter.push_str(body);
    frontmatter.push('\n');

    let inbox_dir = paths.project_ccteam_dir(slug).join("inbox");
    std::fs::create_dir_all(&inbox_dir)
        .with_context(|| format!("create {}", inbox_dir.display()))?;
    let stamp = now.format("%Y%m%dT%H%M%SZ");
    // Pick a sequence so a single second's bulk send doesn't collide.
    let mut seq = 1u32;
    let path = loop {
        let candidate = inbox_dir.join(format!("msg-{stamp}-{:03}.md", seq));
        if !candidate.exists() {
            break candidate;
        }
        seq = seq.saturating_add(1);
        if seq > 999 {
            anyhow::bail!("ccteam send: filename collision storm (>999 in one second)");
        }
    };
    // Atomic write: .tmp then rename (matches inbox.rs::save).
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, &frontmatter)
        .with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("rename {} → {}", tmp.display(), path.display()))?;
    println!("queued inbox message: {}", path.display());
    if no_spawn {
        println!("  (no_spawn: true → archived only, no auto-spawn)");
    } else if let Some(r) = role {
        println!("  target_role: {r}");
    } else {
        println!("  → first workflow `trigger: manual` role on next tick");
    }
    Ok(())
}

fn run_spawn(
    paths: &CcteamPaths,
    slug: &str,
    role: &str,
    prompt: Option<&str>,
) -> Result<()> {
    let project_dir = paths.project_dir(slug);
    if !project_dir.exists() {
        anyhow::bail!(
            "no project `{slug}` at {}",
            project_dir.display()
        );
    }
    // Validate the role exists in workflow.yaml so we fail loud here
    // instead of letting the orchestrator silently delete the marker
    // ("spawn_request for unknown role; deleting").
    let spec = ccteam_core::workflow::WorkflowSpec::load_for_project(&project_dir)
        .with_context(|| format!("load workflow.yaml from {}", project_dir.display()))?;
    if !spec.agents.contains_key(role) {
        anyhow::bail!(
            "role `{role}` not declared in workflow.yaml. Declared roles: {:?}",
            spec.agents.keys().collect::<Vec<_>>()
        );
    }

    let bucket = project_dir.join(".ccteam").join("spawn_requests");
    std::fs::create_dir_all(&bucket)
        .with_context(|| format!("create {}", bucket.display()))?;
    let session_id = format!(
        "{role}-{}",
        chrono::Utc::now().timestamp_micros()
    );
    let marker = bucket.join(format!("{session_id}.json"));

    let resolved_prompt = match prompt {
        Some("-") => Some(read_body_or_stdin("-")?.trim().to_string()),
        Some(p) => Some(p.to_string()),
        None => None,
    };
    let mut payload = serde_json::json!({
        "role": role,
        "session_id": session_id,
        "source": "cli",
        "requested_at": chrono::Utc::now().to_rfc3339(),
    });
    if let Some(p) = resolved_prompt.as_deref() {
        payload["prompt"] = serde_json::Value::String(p.to_string());
    }
    std::fs::write(&marker, serde_json::to_string_pretty(&payload)?)
        .with_context(|| format!("write {}", marker.display()))?;
    println!("queued spawn request: {}", marker.display());
    println!("  role:       {role}");
    println!("  session_id: {session_id}");
    if let Some(p) = resolved_prompt.as_deref() {
        let head: String = p.chars().take(80).collect();
        println!("  prompt:     {head}{}", if p.len() > 80 { "…" } else { "" });
    } else {
        println!("  prompt:     <default kick prompt>");
    }
    Ok(())
}

fn run_new(
    request: Option<String>,
    file: Option<PathBuf>,
    team: String,
    slug: Option<String>,
    no_auto_slug: bool,
    auto_slug_model: String,
) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    let body = match (file, request) {
        (Some(path), _) => {
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?
        }
        (None, Some(text)) => text,
        (None, None) => {
            anyhow::bail!("ccteam new: provide a request as a positional arg or --file PATH")
        }
    };
    let opts = commands::RunNewOptions {
        slug: slug.as_deref(),
        no_auto_slug,
        auto_slug_model: &auto_slug_model,
    };
    let slug = commands::run_new(&paths, body.trim(), &team, opts)?;
    println!("created project {slug} (team: {team})");
    println!(
        "  spec   : {}",
        paths.project_ccteam_dir(&slug).join("spec.md").display()
    );
    println!("  state  : {}", paths.project_state(&slug).display());
    println!(
        "  config : {}",
        paths
            .project_dir(&slug)
            .join(".claude/settings.json")
            .display()
    );
    println!(
        "\nrun `ccteam start --foreground` (in another terminal) to dispatch the first phase."
    );
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
    let paths = CcteamPaths::from_env()?;
    let body = commands::run_peek(&paths, slug)?;
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
