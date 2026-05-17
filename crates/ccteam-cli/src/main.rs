//! `ccteam` binary entry point.

mod clipboard;
mod commands;
mod mcp_serve;
// V0.4.0 F65 — meta-agent MCP workflow tools (7 new). Lives in its own
// module to keep `mcp_serve.rs` focused on the M2.5 protocol surface
// while the workflow tools accumulate in lockstep with the F66
// orchestrator.
mod mcp_workflow_tools;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use ccteam_core::{CcteamPaths, Orchestrator, OrchestratorConfig};
use commands::{InitMode, InitOptions, OutputFormat};

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
    /// One-shot setup: create `~/.ccteam/` skeleton, stamp the per-project
    /// `workflow.yaml` + `.claude/agents/` scaffolds, and run a quick
    /// health check (claude / tmux / ccteam-on-PATH). Idempotent —
    /// safe to re-run.
    ///
    /// V0.4.2 F72: `ccteam init` is the unified project install.
    /// Defaults to installing in the current working directory (slug =
    /// cwd basename). Use `--in <path>` to install elsewhere, `--slug
    /// <name>` to override the derived slug. Re-running on a directory
    /// that's already a ccteam project refreshes state.json and the
    /// settings.json marker section but preserves `workflow.yaml` and
    /// `.claude/agents/*.md` (use `--force` to overwrite them).
    ///
    /// V0.4.1: pass `-i` / `--interactive` to prompt y/n for optional
    /// global installs (MCP, skill, meta-agent), or `-y` / `--yes`
    /// to install all of them without prompting.
    Init {
        /// V0.4.2 F72: install in this directory instead of the cwd.
        /// Created if absent. Combine with `--slug` to override the
        /// auto-derived slug (default: dir basename).
        #[arg(long, value_name = "PATH")]
        r#in: Option<PathBuf>,
        /// V0.4.2 F72: explicit slug. When `--in` is absent and `--slug`
        /// is given, ccteam installs at `<projects_root>/<slug>/`. When
        /// `--in` is given, the slug overrides the dir-basename default.
        #[arg(long, value_name = "NAME")]
        slug: Option<String>,
        /// V0.4.2 F72: team name for new installs (default `dev`).
        /// Ignored on refresh — existing state.json::team is preserved
        /// unless `--force`.
        #[arg(long, value_name = "NAME")]
        team: Option<String>,
        /// Overwrite every ccteam-managed file: state.json, settings.json
        /// marker section, workflow.yaml, .claude/agents/*.md, and
        /// global helper templates. Without `--force` re-runs preserve
        /// user-edited workflow.yaml + agents.
        #[arg(long, default_value_t = false)]
        force: bool,
        /// V0.4.2 F72: only overwrite `.claude/agents/*.md` (keep
        /// workflow.yaml + everything else). Use when the user edited
        /// agents into something broken but wants to keep the workflow
        /// shape.
        #[arg(long, default_value_t = false)]
        reset_agents: bool,
        /// V0.4.1: interactively prompt for each optional install step
        /// after the directory skeleton.
        #[arg(short = 'i', long, default_value_t = false)]
        interactive: bool,
        /// V0.4.1: assume yes for every install-step prompt the wizard
        /// would ask. Useful in scripts / CI.
        #[arg(short = 'y', long, default_value_t = false)]
        yes: bool,
        /// V0.5.0 F93b: workflow mode for the scaffolded workflow.yaml.
        /// `artifact-driven` (default) writes the V0.4.6 trigger-graph
        /// template. `agent-team` writes the `workflow.agent-team.yaml`
        /// template + `__lead.md` scaffold + `.ccteam/inbox/`.
        #[arg(long, value_enum, default_value_t = InitMode::ArtifactDriven)]
        mode: InitMode,
    },
    /// **DEPRECATED** in V0.4.6 (F89) — moved to `ccteam internal hook`.
    /// Old invocation path is preserved one release for back-compat;
    /// emits a stderr WARN on use and will be removed in V0.5.
    ///
    /// Hook handlers invoked by Claude Code per project settings.json.
    /// Each subcommand reads stdin JSON (the Claude Code hook payload)
    /// and performs its side effect; stdout is normally empty.
    #[command(hide = true)]
    Hook {
        #[command(subcommand)]
        cmd: HookCommand,
    },
    /// Run the orchestrator daemon (and, by default, the web UI on
    /// `127.0.0.1:7331` in the same process). Foreground is the only
    /// supported mode — `ccteam start` is enough; the `--foreground`
    /// flag is accepted for back-compat but no longer required.
    /// Pass `--no-web` to run orchestrator only.
    ///
    /// V0.5.0 F93b: when a positional `<slug>` is supplied AND that
    /// project's workflow.yaml has `mode: agent-team`, `ccteam start`
    /// switches into "spawn the lead session" flow:
    ///   - prints a spawn preview (workflow.yaml summary + lead spec)
    ///   - prompts `[Y/n/attach]` (TTY interactive)
    ///   - on Y → spawn lead bg session + print attach hint
    ///   - on attach → spawn + exec `claude attach <id>`
    ///   - on n → cancel, no side effects
    ///
    /// Use `--no-confirm`/`-y` / `--attach` / `--dry-run` to skip the
    /// prompt in scripted callers.
    Start {
        /// V0.5.0 F93b: project slug to spawn an agent-team lead for.
        /// Omit to run the daemon (V0.4.6 behavior).
        slug: Option<String>,
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
        /// Embedded web UI bind address. Default `0.0.0.0:7331` so
        /// host deployments are LAN-reachable out of the box; auth is
        /// auto-enabled on non-loopback binds and the token lands in
        /// `~/.ccteam/web-token`. Use `127.0.0.1:7331` to restrict to
        /// loopback (auth then disabled).
        #[arg(long, value_name = "ADDR", default_value = "0.0.0.0:7331")]
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
        /// V0.4.6 F88 — disable the auto-clipboard of the web bearer
        /// token. Default behavior probes `xclip` / `wl-copy` /
        /// `pbcopy` / `clip.exe` and copies on first hit; pass this
        /// flag in CI / headless / unattended runs to skip the probe.
        #[arg(long, default_value_t = false)]
        no_clipboard: bool,
        /// V0.5.0 F93b: when used with a positional `<slug>`, skip the
        /// `[Y/n/attach]` confirmation prompt and proceed with the
        /// default `Y` (spawn + print attach hint). Useful for CI /
        /// scripts.
        #[arg(short = 'y', long, default_value_t = false)]
        no_confirm: bool,
        /// V0.5.0 F93b: when used with a positional `<slug>`, skip the
        /// prompt and go straight to the `attach` branch (spawn + exec
        /// `claude attach <id>`).
        #[arg(long, default_value_t = false)]
        attach: bool,
        /// V0.5.0 F93b: when used with a positional `<slug>`, print
        /// the spawn preview + exit. Does not spawn anything.
        #[arg(long, default_value_t = false)]
        dry_run: bool,
        /// V0.5.0 F97: revive an agent-team mode project after a
        /// `ccteam stop --cleanup leave-running` (or after the host
        /// rebooted while the lead was still alive). Reads
        /// `.ccteam/team-snapshot.json::lead_session_id`, probes the
        /// `claude --bg` job's `state.json`, and re-arms the F95 watch
        /// without spawning a new lead. If the prior lead is terminal,
        /// emits a WARN and falls through to the normal
        /// `--mode agent-team` spawn flow.
        #[arg(long, default_value_t = false)]
        restart_team: bool,
    },
    /// V0.4.2 F75: thin wrapper over `ccteam init --in
    /// <projects_root>/<slug>` for users who prefer the "create a
    /// new project somewhere central" mental model. Identical
    /// semantics to running `ccteam init --slug <slug>` from any cwd
    /// — see `ccteam init --help` for the full overwrite-strategy
    /// surface.
    ///
    /// The V0.4.0 free-text request + LLM-auto-slug path was dropped
    /// in V0.4.2: `slug` is required and explicit.
    New {
        /// Project slug. Becomes the dir name under `projects_root`.
        slug: String,
        /// Team for the new install. Default `dev`.
        #[arg(long, default_value = "dev")]
        team: String,
    },
    /// List all known projects.
    Ls {
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
    /// V0.4.1: one-screen aggregate health view. Reports daemon
    /// heartbeat age, every project's slug + age + recent-event time,
    /// the last N progress events merged across projects, and the
    /// embedded-web token (file path + value). Replaces having to grep
    /// `ls` + `progress` + multiple `doctor` checks.
    Status {
        /// How many recent progress events to merge + print.
        #[arg(long, default_value_t = 5)]
        tail: usize,
    },
    /// Show one project's full state, recent events, and artifacts.
    /// With no slug, lists every available slug + a re-run hint.
    Show {
        slug: Option<String>,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
    /// Attach to a project's session.
    ///
    /// V0.5.0 F93b: when `<slug>` points at an agent-team mode project,
    /// reads the lead session id from
    /// `.ccteam/team-snapshot.json::lead_session_id` and execs
    /// `claude attach <id>`. For artifact-driven projects, falls back
    /// to tmux session attach (V0.3.x compat) or the latest
    /// `claude --bg` job id (V0.4.0 default).
    Attach { slug: String },
    /// **DEPRECATED** in V0.4.6 (F89) — moved to `ccteam internal peek`.
    ///
    /// Capture the project's pane content without attaching.
    #[command(hide = true)]
    Peek { slug: String },
    /// **DEPRECATED** in V0.4.6 (F89) — moved to `ccteam internal progress`.
    ///
    /// Print the project's progress.jsonl, optionally tailing.
    #[command(hide = true)]
    Progress {
        slug: String,
        #[arg(long)]
        tail: bool,
    },
    /// **DEPRECATED** in V0.4.6 (F89) — moved to `ccteam internal resume`.
    ///
    /// Resume a paused / escalated project (re-arm phase_state=idle).
    #[command(hide = true)]
    Resume { slug: String },
    /// **DEPRECATED** in V0.4.6 (F89) — moved to `ccteam internal send`.
    ///
    /// Send a free-form message to a project's inbox. Wraps the
    /// MCP-equivalent `send_to_session` so users don't have to
    /// hand-write the markdown frontmatter. Orchestrator auto-routes
    /// the message to a worker (or the meta-agent in V0.4.1+) on
    /// the next tick.
    ///
    /// F87: `disable_help_flag` so a literal `--help` in the body is
    /// not intercepted by clap as the subcommand's own help. Users
    /// who want help should run `ccteam help send` instead.
    /// F89: hidden from top-level help (use `ccteam internal send`).
    #[command(hide = true, disable_help_flag = true)]
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
        /// Message body. Use `-` to read from stdin. Leading hyphens
        /// are accepted as literal text (F87) so `ccteam send <slug>
        /// "--help"` forwards the string to the agent instead of
        /// triggering ccteam's own help.
        #[arg(allow_hyphen_values = true)]
        body: String,
    },
    /// **DEPRECATED** in V0.4.6 (F89) — moved to `ccteam internal spawn`.
    ///
    /// Trigger a fresh spawn of `<role>` in `<slug>` with an optional
    /// kick prompt. Writes a `.ccteam/spawn_requests/<role>-<ts>.json`
    /// marker the orchestrator picks up on its next tick. CLI shortcut
    /// for the MCP `ccteam__spawn_agent` tool.
    ///
    /// F87: `disable_help_flag` so a literal `--help` in the prompt is
    /// not intercepted by clap as the subcommand's own help. Users
    /// who want help should run `ccteam help spawn` instead.
    /// F89: hidden from top-level help (use `ccteam internal spawn`).
    #[command(hide = true, disable_help_flag = true)]
    Spawn {
        /// Project slug.
        slug: String,
        /// Workflow role (must exist in `<project>/workflow.yaml`).
        role: String,
        /// Optional initial prompt. Falls back to the default kick
        /// prompt when omitted (let the role's `.claude/agents/<role>.md`
        /// drive). Use `-` to read from stdin. Leading hyphens are
        /// accepted as literal text (F87).
        #[arg(allow_hyphen_values = true)]
        prompt: Option<String>,
    },
    /// **DEPRECATED** in V0.4.6 (F89) — moved to `ccteam internal mcp-serve`.
    ///
    /// M2.5: run the ccteam MCP server (stdio JSON-RPC). Wired into
    /// `~/.claude.json` `mcpServers.ccteam` by `ccteam doctor
    /// --install-mcp` so daily-driver claude sessions and the meta
    /// agent both see the 9-tool surface (interfaces §12).
    #[command(hide = true)]
    McpServe,
    /// Internal commands — hook handlers + meta-agent / MCP integration
    /// points. Not user-facing day to day; meta-agent and the
    /// `ccteam-control` skill drive these. Run `ccteam internal --help`
    /// for the list.
    Internal {
        #[command(subcommand)]
        cmd: InternalCommand,
    },
    /// Stop the running orchestrator daemon, OR (V0.5.0 F97) tear
    /// down one agent-team project's lead session per its workflow.yaml
    /// `cleanup_on_stop:` strategy.
    ///
    /// Without `<slug>`: legacy V0.4.6 behavior — write the per-user
    /// graceful shutdown trigger; daemon drains every project
    /// gracefully; tmux sessions are NOT killed.
    ///
    /// With `<slug>` (V0.5.0 F97): dispatch on the project's
    /// `workflow.yaml::agent_team.cleanup_on_stop`:
    ///   - `force-kill` (default, V0.4.6 compat): SIGKILL the lead
    ///     bg job + clear `.ccteam/team-snapshot.json`.
    ///   - `ask-lead`: write a user-turn cleanup message into
    ///     `.ccteam/inbox/`; wait up to `--stop-timeout` seconds for
    ///     the lead to emit `workflow_done`; on timeout fall back to
    ///     `force-kill` with a WARN.
    ///   - `leave-running`: drop the F95 watch entries + mark the
    ///     project `detached: true` in `state.json`, but leave the lead
    ///     bg job + teammates alive. Subsequent `ccteam start <slug>`
    ///     refuses unless `--restart-team` is set.
    Stop {
        /// V0.5.0 F97: per-slug stop. Reads
        /// `<project>/.ccteam/workflow.yaml::agent_team.cleanup_on_stop`
        /// to choose the strategy. Without this argument the daemon
        /// shutdown trigger is written (legacy V0.4.6 behavior).
        slug: Option<String>,
        /// V0.5.0 F97: seconds to wait for the lead to emit
        /// `workflow_done` when `cleanup_on_stop: ask-lead`. After this
        /// budget elapses, ccteam falls back to `force-kill` with a WARN.
        /// Ignored for other cleanup strategies.
        #[arg(long, value_name = "SECONDS", default_value_t = 60)]
        stop_timeout: u64,
    },
    /// V0.4.6 F81 — un-roster a project: drop the slug from
    /// `~/.ccteam/config.yaml::projects[]`, scrub the orchestration
    /// state (`~/.ccteam/progress/<slug>.jsonl`, `~/.ccteam/inbox/<slug>/`,
    /// `~/.ccteam/control/<slug>/`), and ask the running daemon to
    /// hot-unroster the loop (F82 wiring). With `--purge`, also
    /// `rm -rf <project>/.ccteam/`, `<project>/.claude/agents/`, and
    /// `<project>/workflow.yaml` (and `.ccteam/workflow.yaml` per F83).
    ///
    /// **Red lines** (CLAUDE.md §三):
    /// - Refuses when an active tmux session, claude --bg job, or
    ///   open `agent_spawn` row points at the project. `--force`
    ///   overrides this guard.
    /// - **Never deletes `<project>/.env`** — user secrets stay
    ///   regardless of `--purge`.
    /// - **Never touches business code** — only ccteam-managed paths
    ///   under `.ccteam/`, `.claude/agents/`, and `workflow.yaml`.
    Remove {
        /// Project slug as listed in `ccteam ls` / registered under
        /// `~/.ccteam/config.yaml::projects[]`.
        slug: String,
        /// Also delete `<project>/.ccteam/`, `<project>/.claude/agents/`,
        /// and `<project>/workflow.yaml` (and `.ccteam/workflow.yaml`
        /// per F83). Default leaves the project directory contents
        /// alone (config-only deregister, equivalent to the
        /// "abandon" verb in the PRD).
        #[arg(long, default_value_t = false)]
        purge: bool,
        /// Print every step that would change the filesystem / config /
        /// daemon roster, but don't touch anything. Combine with
        /// `--purge` to see the full clobber list.
        #[arg(long, default_value_t = false)]
        dry_run: bool,
        /// Skip the CLAUDE.md §三 "永不主动 kill 长 session" refusal
        /// gate (tmux / claude bg / open spawn checks). Use only when
        /// you've already drained the project's live work and the
        /// guard is a false positive.
        #[arg(long, default_value_t = false)]
        force: bool,
    },
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
        /// Install ccteam skills under `~/.claude/skills/<name>/SKILL.md`.
        /// M1.8 + V0.5.0 F93a. Default (no value) installs every shipped
        /// skill (`ccteam-control` / `ccteam-team-author` /
        /// `ccteam-project-creator` / `ccteam-team`). Pass a single skill
        /// name (e.g. `--install-skill ccteam-team`) to install just one.
        /// Pass `--install-skill all` for the explicit default. Idempotent.
        #[arg(
            long,
            value_name = "NAME",
            num_args = 0..=1,
            default_missing_value = "all",
        )]
        install_skill: Option<String>,
        /// Bootstrap the canonical meta-agent project at
        /// `~/projects/meta/` with the dispatcher role prompt +
        /// inbox/outbox dirs. Also installs the `ccteam-control` skill.
        /// V0.4.1: handle dropped (one ccteam install = one meta-agent).
        #[arg(long, default_value_t = false)]
        install_meta_agent: bool,
        /// M2.5: register `mcpServers.ccteam` in `~/.claude.json` so
        /// daily-driver claude + meta-agent both see the ccteam MCP
        /// server (9 tools, interfaces §12). Idempotent — overwrites
        /// any prior `ccteam` server entry but preserves other servers.
        #[arg(long, default_value_t = false)]
        install_mcp: bool,
        /// V0.4.1: aggregate first-run setup. Equivalent to
        /// `--install-mcp --install-skill --install-meta-agent`.
        /// Idempotent. (Pre-V0.4.1 needed a `<HANDLE>` value;
        /// handle was dropped — one ccteam install = one meta-agent.)
        #[arg(long, default_value_t = false)]
        install_all: bool,
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
        /// V0.4.2 F74: fold V0.4.1 project layout into the new
        /// `~/.ccteam/config.yaml`. Walks `~/projects/*` and appends
        /// every parseable `.ccteam/state.json` to `config.yaml::
        /// projects[]`; folds `~/.ccteam/watchdog.yaml` into
        /// `config.yaml::watchdog` and renames the old file to
        /// `watchdog.yaml.migrated`. Idempotent.
        #[arg(long, default_value_t = false)]
        migrate_v041_to_v042: bool,
        /// V0.4.6 F83: move every registered project's root
        /// `workflow.yaml` into `<project>/.ccteam/workflow.yaml`.
        /// Default is dry-run; pair with `--apply` to actually move
        /// the files. Conflicts (both locations populated) are
        /// fail-safe — neither file is touched and the user is told
        /// to resolve by hand. Idempotent.
        #[arg(long, default_value_t = false)]
        migrate_workflow_to_ccteam_dir: bool,
        /// V0.4.6 F85: reclaim terminated `~/.claude/jobs/<id>/`
        /// directories older than
        /// `~/.ccteam/config.yaml::claude_jobs_retention_days` (default
        /// 7 days). Default is dry-run — prints what would be removed
        /// without touching disk. Pair with `--apply` to actually
        /// `rm -rf` eligible entries. Never touches dirs whose
        /// `state.json::state == "working"` or whose `state.json` is
        /// missing / unparseable.
        #[arg(long, default_value_t = false)]
        gc_claude_jobs: bool,
        /// V0.4.6 F91: walk every registered project's
        /// `.claude/settings.json` and strip the legacy
        /// `ccteam hook cost-accumulate` PostToolUse entry. Idempotent —
        /// re-runs after success are no-ops. Pair with `--dry-run` to
        /// preview the scrub.
        #[arg(long, default_value_t = false)]
        update_hooks: bool,
        /// V0.5.0 F92: print the embedded `pricing.json` schema_version
        /// next to today's date and WARN when the table is older than
        /// 180 days. No fs mutation; pure readout to remind operators
        /// to upgrade ccteam when the bundled rate sheet ages out.
        #[arg(long, default_value_t = false)]
        check_pricing_version: bool,
        /// V0.4.6 F83/F85: pair with `--migrate-workflow-to-ccteam-dir`
        /// or `--gc-claude-jobs` to commit changes to disk instead of
        /// previewing them. Without it, those subcommands run as
        /// dry-run.
        #[arg(long, default_value_t = false)]
        apply: bool,
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
        /// Listen address. Default `0.0.0.0:7331` so host deployments
        /// reach the LAN out of the box; auth is auto-enabled on
        /// non-loopback. Use `127.0.0.1:7331` for loopback-only
        /// (auth then disabled). M5.3 requires token auth on
        /// non-loopback unless `--no-auth`.
        #[arg(long, default_value = "0.0.0.0:7331")]
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

/// V0.4.6 F89: subcommands hidden under `ccteam internal`. Each mirrors
/// a former top-level command 1:1 — the old top-level names stay as
/// hidden aliases that emit a one-line stderr deprecation WARN and route
/// to the same handler. V0.5 will retire the top-level aliases.
#[derive(Subcommand)]
enum InternalCommand {
    /// Hook handlers invoked by Claude Code per project settings.json.
    /// Each subcommand reads stdin JSON (the Claude Code hook payload)
    /// and performs its side effect; stdout is normally empty.
    Hook {
        #[command(subcommand)]
        cmd: HookCommand,
    },
    /// Run the ccteam MCP server (stdio JSON-RPC). Wired into
    /// `~/.claude.json` `mcpServers.ccteam` by `ccteam doctor
    /// --install-mcp` so daily-driver claude sessions and the meta
    /// agent both see the 17-tool surface (interfaces §12).
    McpServe,
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
    /// Send a free-form message to a project's inbox.
    Send {
        slug: String,
        #[arg(short = 'r', long)]
        role: Option<String>,
        #[arg(long, default_value_t = false)]
        no_spawn: bool,
        body: String,
    },
    /// Trigger a fresh spawn of `<role>` in `<slug>` with an optional
    /// kick prompt. Writes a `.ccteam/spawn_requests/<role>-<ts>.json`
    /// marker the orchestrator picks up on its next tick.
    Spawn {
        slug: String,
        role: String,
        prompt: Option<String>,
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
    /// SessionStart hook: write the `<project>/.ccteam/ready` marker.
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
        Command::Init {
            r#in,
            slug,
            team,
            force,
            reset_agents,
            interactive,
            yes,
            mode,
        } => {
            let paths = CcteamPaths::from_env()?;
            let report = commands::run_init(
                &paths,
                InitOptions {
                    install_in: r#in,
                    slug,
                    team,
                    force,
                    reset_agents,
                    interactive,
                    yes,
                    mode,
                },
            )?;
            print!("{report}");
            Ok(())
        }
        Command::Hook { cmd } => {
            warn_deprecated_top_level("hook", "internal hook");
            run_hook(cmd)
        }
        Command::Start {
            slug,
            foreground: _,
            tick_seconds,
            skip_tool_check,
            claude_argv,
            no_web,
            web_bind,
            web_no_auth,
            web_token_file,
            no_clipboard,
            no_confirm,
            attach,
            dry_run,
            restart_team,
        } => match slug {
            Some(s) => {
                // V0.5.0 F93b: per-slug spawn flow. Reads workflow.yaml
                // to decide; agent-team mode does the [Y/n/attach]
                // spawn; artifact-driven mode bails with friendly hint.
                // V0.5.0 F97: --restart-team revives a `leave-running`
                // detached lead by re-arming the watch without spawning.
                let paths = CcteamPaths::from_env()?;
                let body = commands::run_start_agent_team(
                    &paths,
                    &s,
                    commands::StartAgentTeamOptions {
                        no_confirm,
                        attach,
                        dry_run,
                        restart_team,
                    },
                )?;
                if dry_run {
                    print!("{body}");
                }
                Ok(())
            }
            None => run_start(
                tick_seconds,
                skip_tool_check,
                claude_argv,
                StartWebOpts {
                    disabled: no_web,
                    bind: web_bind,
                    no_auth: web_no_auth,
                    token_file: web_token_file,
                    no_clipboard,
                },
            ),
        },
        Command::New { slug, team } => run_new(slug, team),
        Command::Ls { format } => run_ls(format),
        Command::Status { tail } => run_status(tail),
        Command::Show { slug, format } => match slug {
            Some(s) => run_show(&s, format),
            None => show_slug_picker(),
        },
        Command::Attach { slug } => run_attach(&slug),
        Command::Peek { slug } => {
            warn_deprecated_top_level("peek", "internal peek");
            run_peek(&slug)
        }
        Command::Progress { slug, tail } => {
            warn_deprecated_top_level("progress", "internal progress");
            run_progress(&slug, tail)
        }
        Command::Resume { slug } => {
            warn_deprecated_top_level("resume", "internal resume");
            run_resume(&slug)
        }
        Command::Send {
            slug,
            role,
            no_spawn,
            body,
        } => {
            warn_deprecated_top_level("send", "internal send");
            let paths = CcteamPaths::from_env()?;
            run_send(&paths, &slug, role.as_deref(), no_spawn, &body)
        }
        Command::Spawn { slug, role, prompt } => {
            warn_deprecated_top_level("spawn", "internal spawn");
            let paths = CcteamPaths::from_env()?;
            run_spawn(&paths, &slug, &role, prompt.as_deref())
        }
        Command::McpServe => {
            warn_deprecated_top_level("mcp-serve", "internal mcp-serve");
            run_mcp_serve()
        }
        Command::Internal { cmd } => run_internal(cmd),
        Command::Stop { slug, stop_timeout } => match slug {
            // V0.5.0 F97 — per-slug agent-team cleanup.
            Some(s) => {
                let paths = CcteamPaths::from_env()?;
                let body = commands::run_stop_slug(
                    &paths,
                    &s,
                    commands::StopSlugOptions {
                        stop_timeout: Duration::from_secs(stop_timeout),
                    },
                )?;
                print!("{body}");
                Ok(())
            }
            // V0.4.6 daemon graceful shutdown.
            None => run_stop(),
        },
        Command::Remove {
            slug,
            purge,
            dry_run,
            force,
        } => {
            let paths = CcteamPaths::from_env()?;
            let report = commands::run_remove(
                &paths,
                &slug,
                commands::RemoveOptions {
                    purge,
                    dry_run,
                    force,
                },
            )?;
            print!("{report}");
            Ok(())
        }
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
            migrate_v041_to_v042,
            migrate_workflow_to_ccteam_dir,
            gc_claude_jobs,
            update_hooks,
            check_pricing_version,
            apply,
        } => {
            // V0.4.1 `--install-all` is sugar for the three first-run
            // flags. Explicit flags still win where set; we OR them.
            //
            // V0.5.0 F93a: `install_skill` is now `Option<String>`. The
            // flag is "passed" when `Some(_)`; we keep a derived bool
            // so `--install-all` + `--install-meta-agent` plumbing
            // doesn't have to chase the optional name through every
            // call site.
            let final_mcp = install_mcp || install_all;
            let final_skill = install_skill.is_some() || install_all;
            // None = caller didn't pass --install-skill at all; treat
            // --install-all as the implicit "all". A non-"all" value
            // narrows the selection to a single shipped skill.
            let install_skill_only = install_skill
                .as_deref()
                .filter(|s| !s.eq_ignore_ascii_case("all"))
                .map(|s| s.to_string());
            let final_meta = install_meta_agent || install_all;
            // V0.4.6 F83 + F85: `--apply` inverts default dry-run for
            // both --migrate-workflow-to-ccteam-dir and --gc-claude-jobs
            // so users previewing safely don't accidentally mutate disk.
            // `--dry-run` still wins if explicitly set.
            let f83_dry_run = if migrate_workflow_to_ccteam_dir {
                dry_run || !apply
            } else {
                dry_run
            };
            run_doctor(commands::DoctorOptions {
                dry_run: f83_dry_run,
                force,
                tool_surface,
                install_skill: final_skill,
                install_skill_only,
                install_meta_agent: final_meta,
                install_mcp: final_mcp,
                install_memory_bridge,
                reset_shipped_teams,
                validate_team,
                migrate_recommended_agents,
                screenshot_smoke,
                migrate_v041_to_v042,
                migrate_workflow_to_ccteam_dir,
                gc_claude_jobs,
                gc_apply: apply,
                update_hooks,
                check_pricing_version,
            })
        }
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

/// V0.4.6 F89 — dispatch `ccteam internal <subcmd>`. Mirrors the
/// old top-level commands 1:1; old top-level entry points emit a
/// stderr deprecation WARN before reaching the same handlers.
fn run_internal(cmd: InternalCommand) -> Result<()> {
    match cmd {
        InternalCommand::Hook { cmd } => run_hook(cmd),
        InternalCommand::McpServe => run_mcp_serve(),
        InternalCommand::Attach { slug } => run_attach(&slug),
        InternalCommand::Peek { slug } => run_peek(&slug),
        InternalCommand::Progress { slug, tail } => run_progress(&slug, tail),
        InternalCommand::Resume { slug } => run_resume(&slug),
        InternalCommand::Send {
            slug,
            role,
            no_spawn,
            body,
        } => {
            let paths = CcteamPaths::from_env()?;
            run_send(&paths, &slug, role.as_deref(), no_spawn, &body)
        }
        InternalCommand::Spawn { slug, role, prompt } => {
            let paths = CcteamPaths::from_env()?;
            run_spawn(&paths, &slug, &role, prompt.as_deref())
        }
    }
}

/// V0.4.6 F89 — print a stderr deprecation WARN when an old top-level
/// command is used. The handler still runs; V0.5 will remove the old
/// entry points entirely (see `docs/v0-4-6/prd.md` F89).
fn warn_deprecated_top_level(old: &str, new: &str) {
    eprintln!(
        "ccteam: WARN `ccteam {old}` is deprecated; use `ccteam {new}` instead. \
         The legacy alias will be removed in V0.5.",
    );
}

fn run_mcp_serve() -> Result<()> {
    init_tracing();
    let paths = CcteamPaths::from_env()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime for mcp-serve")?;
    runtime.block_on(mcp_serve::run_mcp_serve(paths))
}

fn run_attach(slug: &str) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    commands::run_attach(&paths, slug)
}

fn run_progress(slug: &str, tail: bool) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    commands::run_progress(&paths, slug, tail)
}

fn run_resume(slug: &str) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    commands::run_resume(&paths, slug)
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

/// V0.4.6 F86 — per-user shutdown trigger file. `ccteam stop` writes
/// here; `ccteam start`'s daemon polls for its existence and routes
/// the signal through the orchestrator's cancel-token path (graceful
/// `workflow_done reason="shutdown"` per project) instead of the
/// V0.4.5 SIGTERM + `JoinSet::abort_all()` hard cut.
///
/// Per-user namespace keeps two operators on the same host from
/// stepping on each other's daemons.
fn shutdown_trigger_path() -> PathBuf {
    let user = std::env::var("USER").unwrap_or_else(|_| "ccteam".into());
    PathBuf::from("/tmp").join(format!("ccteam-{user}.shutdown"))
}

fn run_stop() -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    // V0.4.6 F86 — write the trigger file first; the daemon's main
    // loop polls for it and routes through the orchestrator's
    // graceful cancel path. SIGTERM stays as the legacy fallback for
    // systemd / docker stop (the daemon installs the same handler),
    // but `ccteam stop` no longer needs a process signal — the file
    // is enough.
    let pidfile = ccteam_core::pidfile_path(&paths);
    let pid = match ccteam_core::read_pidfile(&pidfile) {
        Ok(pid) if ccteam_core::daemon::pid_alive(pid) => pid,
        _ => {
            println!("ccteam stop: no running orchestrator (pidfile absent or stale).");
            return Ok(());
        }
    };

    let trigger = shutdown_trigger_path();
    std::fs::write(&trigger, format!("{}\n", std::process::id()))
        .with_context(|| format!("write shutdown trigger {}", trigger.display()))?;
    println!(
        "ccteam stop: graceful shutdown trigger written to {}",
        trigger.display()
    );
    println!("ccteam stop: orchestrator pid {pid} will drain projects (≤ 30s)…");

    // Block until the orchestrator actually exits — docker-stop style.
    // The orchestrator removes its pidfile on graceful shutdown, so
    // either an absent pidfile OR `kill -0 <pid>` returning false is
    // proof of exit. V0.4.6 bumps the wait to 35s so the daemon's
    // own 30s graceful timeout + abort-fallback path can complete
    // before we surface a warning. We never escalate to SIGKILL —
    // CLAUDE.md §三 "永不主动 kill 长 session" applies to ccteam's
    // own daemon too.
    let deadline = std::time::Instant::now() + Duration::from_secs(35);
    while std::time::Instant::now() < deadline {
        if !pidfile.exists() || !ccteam_core::daemon::pid_alive(pid) {
            println!("ccteam stop: orchestrator exited.");
            // Best-effort: tidy the trigger file so the next start
            // doesn't instantly shut itself down on a stale flag.
            let _ = std::fs::remove_file(&trigger);
            println!("tmux sessions are NOT killed — `ccteam start` will reattach to them.");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    eprintln!(
        "ccteam stop: pid {pid} still alive after 35s. Check the daemon log; \
         resend with `kill -TERM {pid}` or inspect with `ps -p {pid}`."
    );
    println!("tmux sessions are NOT killed — `ccteam start` will reattach to them.");
    Ok(())
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
    /// V0.4.6 F88 — when true, skip the clipboard probe in
    /// `print_web_banner` and just print the token. CI / unattended.
    no_clipboard: bool,
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

            let unroster_task = {
                let orch = std::sync::Arc::clone(&orchestrator);
                tokio::spawn(poll_unroster_triggers(orch))
            };

            // V0.5.0 F95 — global `~/.claude/teams/` watcher mirrors 5
            // `team_*` events into `~/.ccteam/teams-progress.jsonl` for
            // the web `/teams` tab to consume. Fire-and-forget: the task
            // owns its notify watcher, exits on runtime shutdown.
            let _agent_teams_watcher_task =
                match ccteam_core::AgentTeamsWatcherConfig::from_env() {
                    Ok(cfg) => match ccteam_core::AgentTeamsWatcher::new(cfg) {
                        Ok(watcher) => Some(watcher.start()),
                        Err(err) => {
                            tracing::warn!(
                                ?err,
                                "F95 AgentTeamsWatcher::new failed; team-events disabled",
                            );
                            None
                        }
                    },
                    Err(err) => {
                        tracing::warn!(
                            ?err,
                            "F95 AgentTeamsWatcherConfig::from_env failed; team-events disabled",
                        );
                        None
                    }
                };

            let web_handle = if web.disabled {
                None
            } else {
                let opts = match parse_web_opts(&web) {
                    Ok(o) => o,
                    Err(err) => {
                        signal_task.abort();
                        unroster_task.abort();
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
            unroster_task.abort();
            orch_result
        })
    })();
    ccteam_core::remove_pidfile(&cleanup_paths);
    result
}

async fn wait_for_shutdown_signal() {
    // V0.4.6 F86 — `ccteam stop` writes `/tmp/ccteam-<user>.shutdown`
    // instead of sending SIGTERM. The daemon polls for the file and
    // collapses to the same shutdown path as SIGTERM (orchestrator
    // graceful cancel via Notify channel). SIGTERM is retained for
    // systemd / docker-stop callers; either trigger is sufficient.
    let trigger = shutdown_trigger_path();
    // Drain any stale trigger left by a previous run before we begin
    // polling so we don't insta-shutdown on startup.
    let _ = std::fs::remove_file(&trigger);
    let trigger_poll = async {
        let mut ticker = tokio::time::interval(Duration::from_millis(250));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            if trigger.exists() {
                tracing::info!(
                    path = %trigger.display(),
                    "shutdown trigger file observed (ccteam stop)"
                );
                let _ = std::fs::remove_file(&trigger);
                return;
            }
        }
    };

    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(?err, "could not install SIGTERM handler");
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => tracing::info!("ctrl+c received"),
                    _ = trigger_poll => tracing::info!("shutdown via trigger file"),
                }
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => tracing::info!("ctrl+c received"),
            _ = sigterm.recv() => tracing::info!("SIGTERM received (ccteam stop)"),
            _ = trigger_poll => tracing::info!("shutdown via trigger file"),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => tracing::info!("ctrl+c received"),
            _ = trigger_poll => tracing::info!("shutdown via trigger file"),
        }
    }
}

/// Poll `/tmp/ccteam-<user>.unroster.<slug>` files every 250ms and,
/// for each found, call `unroster_project(CancelReason::Removed)` then
/// remove the trigger. Written by `ccteam remove` (commands::run_remove
/// step 4); mirrors the F86 shutdown trigger pattern.
async fn poll_unroster_triggers(orch: std::sync::Arc<Orchestrator>) {
    let prefix = {
        let user = std::env::var("USER").unwrap_or_else(|_| "ccteam".into());
        format!("ccteam-{user}.unroster.")
    };
    let tmp = std::path::PathBuf::from("/tmp");
    // Drain any stale trigger files left by a previous crashed daemon so
    // a re-created project doesn't get instantly cancelled on startup —
    // mirrors the stale-shutdown-trigger drain in `wait_for_shutdown_signal`.
    if let Ok(entries) = std::fs::read_dir(&tmp) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name.to_string_lossy().starts_with(&prefix) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    let mut ticker = tokio::time::interval(Duration::from_millis(250));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        let entries = match std::fs::read_dir(&tmp) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if let Some(slug) = name_str.strip_prefix(&prefix) {
                let slug = slug.to_string();
                let path = entry.path();
                tracing::info!(slug, "unroster trigger observed; cancelling project loop");
                orch.unroster_project(&slug, ccteam_core::CancelReason::Removed)
                    .await;
                let _ = std::fs::remove_file(&path);
            }
        }
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
    // When bound to the unspecified address (0.0.0.0), substitute the
    // host's hostname in the displayed URL so the line is clickable;
    // 0.0.0.0:port isn't a real destination. Fall back to the literal
    // socket addr if gethostname() fails.
    let display_url_host = if bind.ip().is_unspecified() {
        read_hostname().unwrap_or_else(|| bind.ip().to_string())
    } else {
        bind.ip().to_string()
    };
    let display_url = format!("{scheme}://{display_url_host}:{}", bind.port());
    eprintln!();
    eprintln!("┌─ ccteam web ─────────────────────────────────────────────");
    if loopback || web.no_auth {
        eprintln!("│  URL:   {display_url}/");
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
                eprintln!("│  URL:   {display_url}/?token=ccteam:{hex}");
                // V0.4.6 F88 — auto-copy the token (full `ccteam:<hex>`
                // form so it round-trips into a curl `-H` or a
                // browser URL bar) unless --no-clipboard.
                let token_str = format!("ccteam:{hex}");
                let suffix = if web.no_clipboard {
                    String::new()
                } else {
                    match clipboard::copy_to_clipboard(&token_str) {
                        Some(provider) => format!("  (copied to clipboard via {provider})"),
                        None => "  (clipboard unavailable; copy manually)".to_string(),
                    }
                };
                eprintln!("│  TOKEN: {token_str}{suffix}");
                eprintln!("│  FILE:  {}", token_path.display());
            }
            Err(err) => {
                eprintln!("│  URL:   {display_url}/  (token init failed: {err})");
            }
        }
    }
    eprintln!("│  BIND:  {bind}");
    eprintln!("└──────────────────────────────────────────────────────────");
    eprintln!();
}

/// Read the OS hostname via libc `gethostname`. Returns `None` on
/// syscall failure or a non-UTF8 result.
#[cfg(unix)]
fn read_hostname() -> Option<String> {
    use std::ffi::CStr;
    let mut buf = [0u8; 256];
    // SAFETY: gethostname writes at most `len-1` bytes and NUL-terminates.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut _, buf.len()) };
    if rc != 0 {
        return None;
    }
    // Ensure NUL-termination before scanning the buffer.
    buf[buf.len() - 1] = 0;
    let cstr = CStr::from_bytes_until_nul(&buf).ok()?;
    let s = cstr.to_str().ok()?.to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}
#[cfg(not(unix))]
fn read_hostname() -> Option<String> {
    None
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

fn run_status(tail: usize) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    use ccteam_core::check_daemon_health;

    println!("ccteam status");
    println!();

    // Daemon health
    let health = check_daemon_health(&paths);
    println!("  daemon:  {}", health.describe());
    println!();

    // Projects
    let projects = ccteam_core::queries::collect_projects(&paths).unwrap_or_default();
    if projects.is_empty() {
        println!("  projects: (none — `ccteam new \"<idea>\"` to create one)");
    } else {
        println!("  projects ({}):", projects.len());
        for p in &projects {
            let age = humanize_secs(p.age_seconds);
            let silent = humanize_secs(p.stall_silent_seconds);
            println!(
                "    {:<32}  age {:>8}  last-event {:>8}",
                p.state.slug, age, silent
            );
        }
    }
    println!();

    // V0.4.7 — running sessions across all projects. Reuses the
    // same `active_sessions` core function the web dashboard reads,
    // so CLI / SPA never drift on what they think is alive.
    if !projects.is_empty() {
        let mut rows: Vec<(String, ccteam_core::ActiveSessionInfo)> = Vec::new();
        for p in &projects {
            match ccteam_core::active_sessions(&p.state.slug, &paths) {
                Ok(list) => rows.extend(list.into_iter().map(|s| (p.state.slug.clone(), s))),
                Err(_) => continue,
            }
        }
        if !rows.is_empty() {
            println!("  running sessions ({}):", rows.len());
            for (slug, s) in &rows {
                let model = s.model.as_deref().unwrap_or("—");
                let ctx = s
                    .context_remaining_pct
                    .map(|p| format!("ctx {:>3.0}%", p))
                    .unwrap_or_else(|| "ctx   —".into());
                let age = parse_rfc3339_age_secs(&s.started_at)
                    .map(humanize_secs)
                    .unwrap_or_else(|| "?".into());
                let short_job = s
                    .job_id
                    .as_deref()
                    .map(|j| j.chars().take(8).collect::<String>())
                    .unwrap_or_else(|| "—".into());
                println!(
                    "    {:<24}  {:<10}  {:<8}  {:<22}  {}  ${:>6.2}  {} ago",
                    slug, s.role, short_job, model, ctx, s.cost_usd, age
                );
            }
            println!("    tip: `claude attach <id>` to take over a session live");
            println!();
        }
    }

    // Recent events across all projects (merged)
    if tail > 0 && !projects.is_empty() {
        println!("  recent events (last {}):", tail);
        let mut all_events: Vec<(String, serde_json::Value)> = Vec::new();
        for p in &projects {
            let evs = ccteam_core::progress::read_all_events(&paths.progress_jsonl(&p.state.slug))
                .unwrap_or_default();
            for e in evs {
                all_events.push((p.state.slug.clone(), e));
            }
        }
        // Sort by event ts string (RFC3339 sorts lexicographically).
        all_events.sort_by_key(|(_, e)| {
            e.get("ts")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_default()
        });
        for (slug, evt) in all_events.iter().rev().take(tail).rev() {
            let kind = evt.get("event").and_then(|v| v.as_str()).unwrap_or("?");
            let ts = evt.get("ts").and_then(|v| v.as_str()).unwrap_or("");
            let role = evt.get("role").and_then(|v| v.as_str()).unwrap_or("");
            let role_s = if role.is_empty() {
                String::new()
            } else {
                format!(" role={role}")
            };
            println!("    [{ts}] {slug} {kind}{role_s}");
        }
        println!();
    }

    // Web token
    let token_path = ccteam_web::token::default_token_path(&paths);
    if token_path.exists() {
        match std::fs::read_to_string(&token_path) {
            Ok(hex) => println!(
                "  web token: ccteam:{}  ({})",
                hex.trim(),
                token_path.display()
            ),
            Err(_) => println!("  web token: <unreadable> ({})", token_path.display()),
        }
    } else {
        println!(
            "  web token: <none yet — generated on first non-loopback `ccteam start`> ({})",
            token_path.display()
        );
    }
    Ok(())
}

/// Parse an RFC3339 timestamp + return seconds-since for the
/// status / show table. Returns None on unparseable input.
fn parse_rfc3339_age_secs(ts: &str) -> Option<u64> {
    let dt = chrono::DateTime::parse_from_rfc3339(ts).ok()?;
    let now = chrono::Utc::now();
    let secs = now
        .signed_duration_since(dt.with_timezone(&chrono::Utc))
        .num_seconds();
    if secs < 0 {
        Some(0)
    } else {
        Some(secs as u64)
    }
}

fn humanize_secs(s: u64) -> String {
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86400 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86400)
    }
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
        anyhow::bail!("no project `{slug}` at {}", project_dir.display());
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
    std::fs::write(&tmp, &frontmatter).with_context(|| format!("write {}", tmp.display()))?;
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

fn run_spawn(paths: &CcteamPaths, slug: &str, role: &str, prompt: Option<&str>) -> Result<()> {
    let project_dir = paths.project_dir(slug);
    if !project_dir.exists() {
        anyhow::bail!("no project `{slug}` at {}", project_dir.display());
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
    std::fs::create_dir_all(&bucket).with_context(|| format!("create {}", bucket.display()))?;
    let session_id = format!("{role}-{}", chrono::Utc::now().timestamp_micros());
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
        println!(
            "  prompt:     {head}{}",
            if p.len() > 80 { "…" } else { "" }
        );
    } else {
        println!("  prompt:     <default kick prompt>");
    }
    Ok(())
}

fn run_new(slug: String, team: String) -> Result<()> {
    if team.trim().is_empty() {
        anyhow::bail!("ccteam new: --team must be non-empty");
    }
    // V0.4.3 F76: fail loud at the CLI boundary on invalid slug grammar
    // (whitespace / unicode / leading dash etc.) so we don't spawn
    // `~/projects/<garbage>/` and leave junk for the user to clean up.
    let validated =
        ccteam_core::validate_slug_format(&slug).with_context(|| format!("ccteam new {slug:?}"))?;
    let paths = CcteamPaths::from_env()?;
    // V0.4.2 F75: `ccteam new <slug>` delegates to `ccteam init` with
    // `install_in = <projects_root>/<final_slug>`. F22 invariant: if
    // the user-supplied slug isn't already prefixed with `<team>-`,
    // we prepend automatically so `~/.claude/rules/ccteam-lessons-
    // <team>.md` paths globs still match.
    let team_prefix = format!("{team}-");
    let final_slug = if validated.starts_with(&team_prefix) {
        validated
    } else {
        format!("{team_prefix}{validated}")
    };
    let target = paths.projects_root.join(&final_slug);
    let report = commands::run_init(
        &paths,
        commands::InitOptions {
            install_in: Some(target),
            slug: Some(final_slug),
            team: Some(team),
            ..commands::InitOptions::default()
        },
    )?;
    print!("{report}");
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
