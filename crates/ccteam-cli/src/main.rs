//! `ccteam` binary entry point.

mod clipboard;
mod commands;
mod mcp_serve;
// V0.4.0 F65 — meta-agent MCP workflow tools (7 new). Lives in its own
// module to keep `mcp_serve.rs` focused on the M2.5 protocol surface
// while the workflow tools accumulate in lockstep with the F66
// orchestrator.
mod mcp_workflow_tools;
// V0.6.0 Wave 1 (F108 / F111 / F112) — chat / advise tool stubs +
// ToolGroup enum + CCTEAM_DISABLE_TOOLS filter. Wave 2/3 fills the
// chat / advise dispatch handlers.
mod mcp_advise_tools;
mod mcp_chat_tools;
mod mcp_tool_groups;
// V0.6.1 F128 — `ccteam__admin_change_persona` +
// `ccteam__admin_add_tool` real implementations (mutate
// `.claude/agents/<bot>.md` + emit progress events).
mod mcp_admin_tools;
mod web_chat_bridge;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

use ccteam_core::CcteamPaths;
use commands::{InitMode, InitOptions, OutputFormat};

/// Version string shown by `ccteam --version`: the crate version plus the
/// git commit it was built from (e.g. `0.8.4 (<commit>)`), so a running
/// binary's exact build is identifiable. `CCTEAM_GIT_COMMIT` is set by
/// `build.rs` (falls back to "unknown" when git isn't available at build).
const VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("CCTEAM_GIT_COMMIT"),
    ")"
);

#[derive(Parser)]
#[command(
    name = "ccteam",
    version = VERSION,
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
        /// Explicit project name (registered slug). Overrides the
        /// install-dir basename default. Does NOT change where the
        /// project installs — that stays the cwd (or `--in <path>`). To
        /// create a fresh project under `<projects_root>/<team>-<slug>/`,
        /// use `ccteam new <slug>`.
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
    /// Run the v8.1 gateway daemon (IM gateway plus, by default, the
    /// web UI in the same process). Foreground is the only supported
    /// mode — `ccteam start` is enough; the `--foreground` flag is
    /// accepted for back-compat but no longer required.
    /// Pass `--no-web` to run the gateway without web.
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
        /// Omit to run the v8.1 gateway daemon.
        slug: Option<String>,
        /// Back-compat no-op: foreground is the only mode.
        #[arg(long, default_value_t = false, hide = true)]
        foreground: bool,
        /// Back-compat no-op for no-slug gateway start.
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
        /// only the IM gateway (e.g. headless server, custom web bind
        /// via a separate `ccteam web` invocation).
        #[arg(long, default_value_t = false)]
        no_web: bool,
        /// Skip starting the embedded `ccteam-im` gateway (Telegram /
        /// Slack / Discord bridge). The gateway lives in this process
        /// (no separate `ccteam-im` binary); pass this to run web-only.
        #[arg(long, default_value_t = false)]
        no_imd: bool,
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
    /// <projects_root>/<team>-<slug>` for users who prefer the "create a
    /// new project somewhere central" mental model — it prepends the
    /// team prefix and installs there. (`ccteam init --slug <name>` no
    /// longer relocates: it installs in the cwd and only sets the name.)
    /// See `ccteam init --help` for the full overwrite-strategy surface.
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
    /// List live gateway chat-mode bot sessions (`ccteam-chat-<slug>-<role>`).
    ///
    /// Read-only control-plane enumeration: lists session names from the mux
    /// backend (never capture-pane) and reconciles them against the daemon's
    /// persisted registry, flagging orphans (live but untracked) and registered
    /// sessions that are not running. Attach one with
    /// `ccteam internal attach <slug> [role]`.
    Sessions,
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
    /// Pause auto-dispatch for one project. Sets `user_pause_pending`
    /// so the workflow loop stops handing the project fresh work; never
    /// kills the long-running session (CLAUDE.md §三 red line). Mirrors
    /// the `mcp__ccteam__workflow_pause` tool — the documented
    /// `ccteam-control` skill control surface.
    Pause { slug: String },
    /// Resume a paused / escalated project (re-arm phase_state=idle).
    /// Mirrors the `mcp__ccteam__workflow_resume` tool.
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
    /// V0.8 rmux — mux backend utilities. Today the only subcommand is
    /// `hook-emit`, the W6 daemon-bus hook reroute client (active only
    /// when `CCTEAM_HOOK_VIA_DAEMON=1`; see `ccteam mux hook-emit
    /// --help`).
    Mux {
        #[command(subcommand)]
        cmd: MuxCommand,
    },
    /// Internal commands — hook handlers + meta-agent / MCP integration
    /// points. Not user-facing day to day; meta-agent and the
    /// `ccteam-control` skill drive these. Run `ccteam internal --help`
    /// for the list.
    Internal {
        #[command(subcommand)]
        cmd: InternalCommand,
    },
    /// Stop the running gateway daemon, OR (V0.5.0 F97) tear
    /// down one agent-team project's lead session per its workflow.yaml
    /// `cleanup_on_stop:` strategy.
    ///
    /// Without `<slug>`: write the per-user graceful shutdown trigger;
    /// daemon drains web / IM gateway / MCP socket / hook sink
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
        /// V0.6.0 Wave 3 F112: probe `codex --version` and warn when
        /// older than 0.131 (minimum supported by Wave 3 mode-3 codex
        /// bot path). No fs mutation. Pairs with --check-codex-auth.
        #[arg(long, default_value_t = false)]
        check_codex_version: bool,
        /// V0.6.0 Wave 3 F112: probe `codex login status` and report
        /// whether the operator is logged in to ChatGPT / API. No fs
        /// mutation. Pairs with --check-codex-version.
        #[arg(long, default_value_t = false)]
        check_codex_auth: bool,
        /// V0.6.5 F155: deterministic gate for the `ccteam-creator`
        /// Phase 3.5 Codex auto-critic detection. Probes whether
        /// `codex` (honoring `$CCTEAM_CODEX_BIN`) is available AND emits
        /// well-formed `--json` output, then prints a single JSON line
        /// `{"available": true|false, ...}` to stdout. Exit code: 0 =
        /// deterministic available, 2 = codex unavailable (binary
        /// missing / version probe failed / not authenticated), 3 =
        /// codex available but output malformed (skill must NOT inject
        /// `executor: codex`). The skill consults this subprocess
        /// instead of running `codex --version && codex login status`
        /// inline so the gate is deterministic + testable.
        #[arg(long, default_value_t = false)]
        check_codex_auto_critic: bool,
        /// V0.6.6 F173: reconcile `<ccteam_root>/cost-budget.json` ledger
        /// rows against every registered project's `progress.jsonl`
        /// over the last 24h. Reports "cost orphan: N <vendor> agent_done
        /// events in progress.jsonl, M rows in ledger" for any mismatch
        /// (vendor adapter call recorded a `progress.jsonl` event but no
        /// ledger row). Silent OK when fully reconciled. No fs mutation —
        /// pure-readout invariant check; future regressions (a new vendor
        /// adapter that forgets the ledger hook) surface here.
        #[arg(long, default_value_t = false)]
        check_cost_orphan: bool,
        /// V0.6.1 F139: materialize the `~/.ccteam/hooks/hook.sh`
        /// daemon-aware Claude Code hook dispatcher (idempotent, chmod
        /// 0755). Run after a ccteam binary upgrade to refresh the
        /// script body. `ccteam init` already does this on first
        /// install.
        #[arg(long, default_value_t = false)]
        install_hooks: bool,
        /// V0.6.1 F139: rewrite every registered project's
        /// `.claude/settings.json` so hook commands invoke
        /// `~/.ccteam/hooks/hook.sh` instead of the V0.4.6 / V0.6.0
        /// `<ccteam-bin> internal hook ...` form (or older `cct hook
        /// ...` / `ccteam hook ...` forms). Idempotent; pair with
        /// `--dry-run` to preview without writing.
        #[arg(long, default_value_t = false)]
        migrate_hook_commands: bool,
        /// V0.4.6 F83/F85: pair with `--migrate-workflow-to-ccteam-dir`
        /// or `--gc-claude-jobs` to commit changes to disk instead of
        /// previewing them. Without it, those subcommands run as
        /// dry-run.
        #[arg(long, default_value_t = false)]
        apply: bool,
        /// V0.6.6 F171: assert the MCP tool surface is fully wired.
        /// Counts active vs STUB tools (cross-checked against
        /// `mcp_tool_groups::STUB_TOOLS`) and exits 1 when any STUB
        /// is registered. Pair with `--json` for machine-readable
        /// output (single JSON object on stdout). Use to gate CI on
        /// the "0 STUB" invariant V0.6.5 ship-gate item #9 introduced.
        #[arg(long, default_value_t = false)]
        verify_mcp: bool,
        /// V0.6.6 F171: emit machine-readable JSON instead of the
        /// human-friendly text report (only used with `--verify-mcp`
        /// today; ignored otherwise).
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// V0.3.1 F49 — adhoc multi-session primitives for `kind: flex`
    /// teams. `add --harness claude` creates a registered tmux session;
    /// `add --harness codex` still returns the V0.3.2 stub error.
    Session {
        #[command(subcommand)]
        action: SessionAction,
    },
    /// V0.3 M5.0: serve the ccteam web UI (read + restricted-write,
    /// `docs/versions/v0-3/prd.md` §3-§6). M5.0 ships the scaffold + `/health`
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
    /// V0.6.0 Wave 3 F112 §C — read / write `~/.ccteam/preferences.toml`.
    /// Today the only user-visible knob is `fallback.on_claude_quota`
    /// (`off` | `codex`); V0.7+ will fold in additional opt-in
    /// preferences. Without args, prints the current preferences.
    Prefs {
        #[command(subcommand)]
        action: Option<PrefsAction>,
    },
    /// V0.6.1 F128 — chat-mode bot admin operations exposed as a
    /// CLI surface for parity with the MCP tools
    /// (`mcp__ccteam__admin_change_persona` /
    /// `mcp__ccteam__admin_add_tool`). The `/ccteam-control` skill
    /// is the primary user-facing entry point; this CLI form is the
    /// scripted fallback when MCP is not registered.
    Admin {
        #[command(subcommand)]
        action: AdminAction,
    },
    /// V0.6.6 F167 — probe a repo root and emit the detected
    /// project kind (monorepo / single-repo / docs-only / scripts-only
    /// / empty), the top-3 languages, a tests-present flag, and the
    /// `probable_scope` paths the `/ccteam-creator` skill should
    /// pre-populate into the rendered `workflow.yaml::agents.<role>.
    /// scope` field. Pure read-only file-existence sweep — no source
    /// parsing, no LLM calls.
    ///
    /// Skill use: invoked by `/ccteam-creator` Phase 3.6 before the
    /// PROJECT PLAN render so the user sees sensible scope defaults
    /// without having to hand-edit yaml after the install.
    ProbeProject {
        /// Repo root to probe. Defaults to cwd.
        #[arg(long, value_name = "PATH")]
        path: Option<PathBuf>,
        /// Emit the probe as a JSON object (stable schema — see
        /// `commands::run_probe_project` docs). Default is a
        /// 4-line human-readable summary.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
}

/// V0.6.1 F128 — `ccteam admin` subcommand surface (chat-mode bot
/// persona / tool-list editing). Mirrors
/// `mcp__ccteam__admin_change_persona` / `mcp__ccteam__admin_add_tool`.
#[derive(Subcommand)]
enum AdminAction {
    /// Replace a chat-mode bot's persona file
    /// (`<project>/.claude/agents/<bot>.md`). `new_persona_md` is the
    /// FULL replacement file content (YAML frontmatter + body); the
    /// caller is responsible for assembling it. The bot picks up the
    /// new persona on the next turn / `/clear`.
    ChangePersona {
        /// Project slug.
        slug: String,
        /// Bot persona id (matches `.claude/agents/<bot>.md`).
        bot: String,
        /// Complete replacement file content. Use `-` to read from
        /// stdin (skill / scripted callers prefer this so multi-line
        /// markdown does not bloat the argv).
        new_persona_md: String,
    },
    /// Append a tool to a chat-mode bot's `.claude/agents/<bot>.md`
    /// frontmatter `tools:` CSV. Idempotent — re-adding an existing
    /// tool is a no-op. Bot picks up the new list on the next turn.
    AddTool {
        /// Project slug.
        slug: String,
        /// Bot persona id.
        bot: String,
        /// Tool descriptor (verbatim — taken as-is into the CSV).
        tool_descriptor: String,
    },
    /// V0.6.8 F202 — register a chat-mode bot directly via the CLI.
    /// Mirrors the `mcp__ccteam__chat_register_bot` MCP tool. Useful as
    /// a scripted / no-daemon fallback when the MCP server isn't
    /// registered yet (first-time setup, automation, MCP-less envs).
    RegisterBot {
        /// Project slug (workflow.yaml's `name` field).
        #[arg(long)]
        slug: String,
        /// Role within the workflow (matches the `agents.<role>` key).
        #[arg(long)]
        role: String,
        /// Harness vendor — `claude` or `codex`.
        #[arg(long, default_value = "claude")]
        vendor: String,
        /// IM platform — `telegram`, `slack`, `discord`, or `mock`.
        #[arg(long, default_value = "telegram")]
        platform: String,
        /// Platform chat id (Telegram chat_id, Slack channel id, etc.).
        /// `allow_hyphen_values` is required because Telegram super-group /
        /// channel chat ids are negative integers (e.g. `-1001234567890`);
        /// without this clap parses `-1...` as an unknown flag.
        #[arg(long, allow_hyphen_values = true)]
        chat_id: String,
        /// Optional IM mention this bot answers to (without leading
        /// `@`). When omitted, auto-mints an unused scientist nickname
        /// from `ccteam_core::agent_naming::SCIENTIST_NAMES`. Letters,
        /// digits, `_`, `-` only.
        #[arg(long)]
        chat_handle: Option<String>,
        /// Optional absolute path to the project hosting
        /// `.ccteam/workflow.yaml`. Defaults to cwd (canonicalized).
        #[arg(long)]
        project_dir: Option<PathBuf>,
    },
    /// V0.6.8 F202 — unregister a chat-mode bot. Mirrors the
    /// `mcp__ccteam__chat_unregister_bot` MCP tool. Idempotent —
    /// returns `ok:true, removed:false` when no registration exists.
    UnregisterBot {
        /// Project slug.
        #[arg(long)]
        slug: String,
        /// Role within the workflow.
        #[arg(long)]
        role: String,
    },
    /// List registered chat-mode bots (reads the F146 registry at
    /// `~/.ccteam/imd/registry/<slug>/<role>.json`). Confirms what
    /// `register-bot` wrote — role → @handle → platform/chat_id, plus
    /// live `running` status from the per-bot heartbeat sidecar.
    /// Note: distinct from the MCP `admin_ls` tool, which lists
    /// *projects*, not bot registrations.
    ListBots {
        /// Optional slug filter. Omit to list bots across all slugs.
        #[arg(long)]
        slug: Option<String>,
        /// Emit a JSON array instead of the human-readable table.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
}

/// V0.6.0 Wave 3 F112 §C — `ccteam prefs` subcommand surface.
#[derive(Subcommand)]
enum PrefsAction {
    /// Pretty-print the active preferences (defaults shown when the
    /// file is absent).
    Show,
    /// Look up a single preference by dotted key. Supported keys today:
    ///   - `fallback.on_claude_quota`
    Get {
        /// Dotted preference key.
        key: String,
    },
    /// Set one preference by dotted key. Writes
    /// `~/.ccteam/preferences.toml` atomically. Supported keys today:
    ///   - `fallback.on_claude_quota`  (values: `off` | `codex`)
    ///   - `fallback.codex.enabled_for_roles` (comma-separated list,
    ///     or empty string to mean "all roles")
    Set {
        /// Dotted preference key.
        key: String,
        /// New value.
        value: String,
    },
}

/// V0.8 rmux W6 — `ccteam mux` subcommand group.
#[derive(Subcommand)]
enum MuxCommand {
    /// W6 daemon-bus hook reroute client. A Claude Code hook subprocess
    /// invokes this (only when the host opted in with
    /// `CCTEAM_HOOK_VIA_DAEMON=1`) to forward its firing to the
    /// orchestrator over `~/.ccteam/run/hook.sock` instead of writing
    /// `progress.jsonl` directly. Reads the hook payload from `--json`
    /// or stdin; derives the session id from `CCTEAM_CHAT_SLUG` /
    /// `CCTEAM_CHAT_ROLE`. Exits 0 on send; exits non-zero but QUIET
    /// when the sink isn't listening so a stray fire never error-spams
    /// Claude Code's UI.
    HookEmit {
        /// Dispatch kind, e.g. `chat-progress`.
        #[arg(long, value_name = "KIND")]
        kind: String,
        /// Dispatch action (the event arg), e.g. `session-start`.
        #[arg(long, value_name = "ACTION")]
        action: Option<String>,
        /// Explicit session id; defaults to `<slug>-<role>` derived from
        /// `CCTEAM_CHAT_SLUG` / `CCTEAM_CHAT_ROLE` env.
        #[arg(long, value_name = "SID")]
        session: Option<String>,
        /// Hook payload JSON inline. When absent, the payload is read
        /// from stdin (`-` is also treated as "read stdin").
        #[arg(long, value_name = "JSON")]
        json: Option<String>,
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
    /// Attach to a session. Resolves a gateway chat-mode bot session
    /// (`ccteam-chat-<slug>-<role>`) first: `<slug> <role>` (or a full session
    /// name) is deterministic; with `<slug>` alone, attaches when the slug has
    /// exactly one live chat session, else lists them to disambiguate. Falls
    /// back to the project session (`ccteam-<slug>`) when no live chat session
    /// matches.
    Attach {
        slug: String,
        /// Chat-mode role (the trailing segment of `ccteam-chat-<slug>-<role>`).
        /// Omit to auto-resolve a single live chat session for `<slug>`.
        role: Option<String>,
    },
    /// Capture the project's pane content without attaching. Resolves a
    /// live chat session (`ccteam-chat-<slug>-<role>`) first, falling back
    /// to the project pane (`ccteam-<slug>`) — same resolution as `attach`.
    Peek {
        slug: String,
        /// Chat-mode role; omit to auto-resolve a single live chat session.
        role: Option<String>,
    },
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
    /// V0.6.0 F108 — `mode: chat` Claude Code hook callback. Each
    /// hook event arg maps to one ccteam `chat_*` progress.jsonl
    /// emission. See `ccteam_hooks::chat_progress` for the dispatch
    /// table.
    ChatProgress {
        /// The hook-event arg (e.g. `session-start`, `user-prompt`,
        /// `stop`, `subagent-stop`, `tool-use`, `session-end`,
        /// `pre-compact`, `post-compact`).
        event: String,
    },
}

fn main() -> Result<()> {
    // V0.8 W2a — rmux SDK daemon spawn protocol. BEFORE clap parses
    // argv, intercept the `--__internal-daemon <socket>` form emitted
    // by `rmux_sdk::Rmux::connect_or_start` so the same ccteam binary
    // can host the rmux daemon. See
    // `docs/versions/v0-8-rmux/w2-daemon-spawn-protocol.md` and
    // `references/rmux/crates/rmux-sdk/src/handles/rmux/connect.rs`
    // (lines 150-180) for the upstream invariant this implementation
    // tracks.
    //
    // TODO(V0.8-W2c): `RmuxBackend::new()` sets `RMUX_SDK_DAEMON_BINARY`
    // lazily in its constructor — enough for the orchestrator's own
    // use, but child processes ccteam spawns (claude / codex subagents)
    // inherit env at fork time. When W2c migrates `claude_tui.rs` /
    // `codex_exec.rs` to the trait, set `RMUX_SDK_DAEMON_BINARY =
    // current_exe()` here at `main()` entry so inherited-env propagation
    // covers any subagent that later uses rmux-sdk. No subagent uses it
    // today, so W2a leaves this as a TODO rather than a behavior change.
    {
        let raw_args: Vec<std::ffi::OsString> = std::env::args_os().collect();
        if raw_args.len() >= 3 && raw_args[1] == ccteam_harness::daemon::INTERNAL_DAEMON_FLAG {
            let socket = raw_args[2].clone();
            ccteam_harness::daemon::run_internal_daemon(socket)
                .context("ccteam internal rmux daemon")?;
            return Ok(());
        }
    }

    // V0.8 W2c — set `RMUX_SDK_DAEMON_BINARY` to this binary at main()
    // entry (not lazily in RmuxBackend::new) so the value is in the
    // process env BEFORE any child process is forked. Child agents
    // (claude / codex sessions) that later use rmux-sdk inherit it and
    // resolve the same ccteam binary as their daemon host, rather than
    // falling back to a `rmux` binary on PATH that may not exist.
    // Idempotent — honors an explicit operator override.
    if std::env::var_os(ccteam_harness::daemon::SDK_DAEMON_BINARY_ENV).is_none() {
        if let Ok(exe) = std::env::current_exe() {
            std::env::set_var(
                ccteam_harness::daemon::SDK_DAEMON_BINARY_ENV,
                exe.as_os_str(),
            );
        }
    }

    let cli = Cli::parse();

    // F173 — pin `CCTEAM_HOME` early so child spawn paths
    // (CodexExecAdapter ledger hook, etc.) see the same root the CLI
    // resolved. Production users almost never set `CCTEAM_HOME`
    // explicitly; without this seed the cost-budget.json ledger hook
    // in CodexExecAdapter would silently no-op (the adapter gates the
    // hook on `CCTEAM_HOME` explicitly to avoid cargo-test pollution).
    // Idempotent — if the operator already set it, we honour their
    // override.
    if std::env::var_os("CCTEAM_HOME").is_none() {
        if let Ok(paths) = CcteamPaths::from_env() {
            std::env::set_var("CCTEAM_HOME", &paths.root);
        }
    }

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
            no_imd,
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
                StartImdOpts { disabled: no_imd },
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
        Command::Sessions => commands::run_sessions(),
        Command::Peek { slug } => {
            warn_deprecated_top_level("peek", "internal peek");
            run_peek(&slug)
        }
        Command::Progress { slug, tail } => {
            warn_deprecated_top_level("progress", "internal progress");
            run_progress(&slug, tail)
        }
        Command::Pause { slug } => run_pause(&slug),
        Command::Resume { slug } => run_resume(&slug),
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
        Command::Mux { cmd } => run_mux(cmd),
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
            check_codex_version,
            check_codex_auth,
            check_codex_auto_critic,
            check_cost_orphan,
            install_hooks,
            migrate_hook_commands,
            apply,
            verify_mcp,
            json,
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
                check_codex_version,
                check_codex_auth,
                check_codex_auto_critic,
                check_cost_orphan,
                install_hooks,
                migrate_hook_commands,
                verify_mcp,
                verify_mcp_json: json,
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
        Command::Prefs { action } => {
            let paths = CcteamPaths::from_env()?;
            match action {
                None | Some(PrefsAction::Show) => {
                    let out = commands::run_prefs_show(&paths)?;
                    print!("{out}");
                    Ok(())
                }
                Some(PrefsAction::Get { key }) => {
                    let out = commands::run_prefs_get(&paths, &key)?;
                    println!("{out}");
                    Ok(())
                }
                Some(PrefsAction::Set { key, value }) => {
                    let out = commands::run_prefs_set(&paths, &key, &value)?;
                    println!("{out}");
                    Ok(())
                }
            }
        }
        Command::Admin { action } => {
            let paths = CcteamPaths::from_env()?;
            match action {
                AdminAction::ChangePersona {
                    slug,
                    bot,
                    new_persona_md,
                } => {
                    let body = read_inline_or_stdin(&new_persona_md)?;
                    let out = commands::run_admin_change_persona(&paths, &slug, &bot, &body)?;
                    println!("{out}");
                    Ok(())
                }
                AdminAction::AddTool {
                    slug,
                    bot,
                    tool_descriptor,
                } => {
                    let out = commands::run_admin_add_tool(&paths, &slug, &bot, &tool_descriptor)?;
                    println!("{out}");
                    Ok(())
                }
                AdminAction::RegisterBot {
                    slug,
                    role,
                    vendor,
                    platform,
                    chat_id,
                    chat_handle,
                    project_dir,
                } => {
                    let out = commands::run_admin_register_bot(
                        &paths,
                        &slug,
                        &role,
                        &vendor,
                        &platform,
                        &chat_id,
                        chat_handle.as_deref(),
                        project_dir.as_deref(),
                    )?;
                    println!("{out}");
                    Ok(())
                }
                AdminAction::UnregisterBot { slug, role } => {
                    let out = commands::run_admin_unregister_bot(&paths, &slug, &role)?;
                    println!("{out}");
                    Ok(())
                }
                AdminAction::ListBots { slug, json } => {
                    let out = commands::run_admin_list_bots(&paths, slug.as_deref(), json)?;
                    println!("{out}");
                    Ok(())
                }
            }
        }
        Command::ProbeProject { path, json } => {
            let root = match path {
                Some(p) => p,
                None => std::env::current_dir().context("get cwd for probe-project")?,
            };
            // JSON branch already produces no trailing newline (consumers
            // pipe to `jq` — extra newline is noise). Text branch
            // already includes a trailing `\n`. Both safe with `print!`.
            let out = commands::run_probe_project(&root, json)?;
            print!("{out}");
            Ok(())
        }
    }
}

/// V0.6.1 F128 — `ccteam admin change-persona` accepts the persona
/// body either inline (small descriptions) or as `-` to read from
/// stdin (full multi-line markdown without argv bloat). Mirrors the
/// `git commit -F -` convention.
fn read_inline_or_stdin(arg: &str) -> Result<String> {
    if arg == "-" {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("read persona body from stdin")?;
        Ok(buf)
    } else {
        Ok(arg.to_string())
    }
}

/// V0.4.6 F89 — dispatch `ccteam internal <subcmd>`. Mirrors the
/// old top-level commands 1:1; old top-level entry points emit a
/// stderr deprecation WARN before reaching the same handlers.
fn run_internal(cmd: InternalCommand) -> Result<()> {
    match cmd {
        InternalCommand::Hook { cmd } => run_hook(cmd),
        InternalCommand::McpServe => run_mcp_serve(),
        InternalCommand::Attach { slug, role } => run_internal_attach(&slug, role.as_deref()),
        InternalCommand::Peek { slug, role } => run_internal_peek(&slug, role.as_deref()),
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
/// entry points entirely (see `docs/versions/v0-4-6/prd.md` F89).
fn warn_deprecated_top_level(old: &str, new: &str) {
    eprintln!(
        "ccteam: WARN `ccteam {old}` is deprecated; use `ccteam {new}` instead. \
         The legacy alias will be removed in V0.5.",
    );
}

fn run_mcp_serve() -> Result<()> {
    // V0.6.5 F165 — stdio MCP server: stdout is reserved for line-
    // delimited JSON-RPC frames, so tracing must go to stderr (otherwise
    // the first `tools/list` reply is preceded by an `info!` log line
    // and the client's JSON parser blows up). See `init_tracing_stderr`
    // doc comment + `docs/interfaces.md` §12.
    init_tracing_stderr();
    let paths = CcteamPaths::from_env()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime for mcp-serve")?;
    runtime.block_on(mcp_serve::run_mcp_serve(paths))
}

/// V0.8 rmux W6 — `ccteam mux <subcommand>` dispatch.
fn run_mux(cmd: MuxCommand) -> Result<()> {
    match cmd {
        MuxCommand::HookEmit {
            kind,
            action,
            session,
            json,
        } => run_mux_hook_emit(kind, action, session, json),
    }
}

/// V0.8 rmux W6 — connect to `~/.ccteam/run/hook.sock` and forward one
/// hook firing to the orchestrator (single-writer path).
///
/// QUIET-on-failure contract: when the sink isn't listening (no
/// orchestrator running with the flag set, stale socket), we exit
/// non-zero WITHOUT printing to stderr. A Claude Code hook subprocess
/// that error-spammed stderr would pollute the chat UI; a silent
/// non-zero exit is the documented behaviour (Claude Code tolerates a
/// failed fire-and-forget hook).
fn run_mux_hook_emit(
    kind: String,
    action: Option<String>,
    session: Option<String>,
    json: Option<String>,
) -> Result<()> {
    // Derive the session id from env when not given explicitly. The hook
    // subprocess inherits CCTEAM_CHAT_SLUG / CCTEAM_CHAT_ROLE from the
    // chat tmux session (claude_tui::chat_spawn_env_owned). Either may
    // be empty if env propagation failed; that's fine — the orchestrator
    // re-derives routing from the payload the same way the legacy hook
    // handler did, so session_id is informational.
    let session_id = session.unwrap_or_else(|| {
        let slug = std::env::var("CCTEAM_CHAT_SLUG").unwrap_or_default();
        let role = std::env::var("CCTEAM_CHAT_ROLE").unwrap_or_default();
        format!("{slug}-{role}")
    });

    // Payload: `--json` inline (unless it's `-`), else read stdin.
    let payload_json = match json {
        Some(s) if s != "-" => s,
        _ => {
            use std::io::Read;
            let mut buf = String::new();
            // A hook may fire with no stdin (e.g. SessionStart in some
            // configs); tolerate an empty read.
            let _ = std::io::stdin().lock().read_to_string(&mut buf);
            buf
        }
    };

    let event = ccteam_harness::HookEvent {
        session_id,
        kind,
        action,
        payload_json,
    };
    let socket = ccteam_harness::default_ccteam_hook_socket_path();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime for mux hook-emit")?;
    match runtime.block_on(ccteam_harness::HookSinkClient::emit(&socket, &event)) {
        Ok(()) => Ok(()),
        Err(_) => {
            // Quiet non-zero exit — no stderr. See contract above.
            std::process::exit(1);
        }
    }
}

// V0.6.1 F130 — `ccteam daemon {start,stop,status}` removed. The IMD
// supervisor now lives inside `ccteam start` as one tokio task, so
// lifecycle = `ccteam start` / `ccteam stop` (same as orchestrator +
// web). Status check is `ccteam doctor` (heartbeat file probe).

fn run_attach(slug: &str) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    commands::run_attach(&paths, slug)
}

/// `ccteam internal attach <slug> [role]` — reach gateway chat-mode bot
/// sessions first (the project-oriented [`commands::run_attach`] cannot see
/// `ccteam-chat-*`), then fall back to the project session when no live chat
/// session matches `slug`.
fn run_internal_attach(slug: &str, role: Option<&str>) -> Result<()> {
    if commands::try_attach_chat_session(slug, role)? {
        return Ok(());
    }
    let paths = CcteamPaths::from_env()?;
    commands::run_attach(&paths, slug)
}

fn run_progress(slug: &str, tail: bool) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    commands::run_progress(&paths, slug, tail)
}

/// `ccteam pause <slug>` — pause auto-dispatch for one project. Same
/// `actions::pause` body the `workflow_pause` MCP tool and the web
/// action route call; never kills the session (CLAUDE.md §三 red line).
fn run_pause(slug: &str) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    ccteam_core::actions::pause(&paths, slug)?;
    println!("ccteam: paused `{slug}` (no kill — re-arm with `ccteam resume {slug}`)");
    Ok(())
}

fn run_resume(slug: &str) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    commands::run_resume(&paths, slug)?;
    println!("ccteam: resumed `{slug}`");
    Ok(())
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
            println!("ccteam stop: no running gateway daemon (pidfile absent or stale).");
            return Ok(());
        }
    };

    let trigger = shutdown_trigger_path();
    std::fs::write(&trigger, format!("{pid}\n"))
        .with_context(|| format!("write shutdown trigger {}", trigger.display()))?;
    println!(
        "ccteam stop: graceful shutdown trigger written to {}",
        trigger.display()
    );
    println!("ccteam stop: gateway daemon pid {pid} will drain (≤ 5s per task)…");

    // Block until the gateway daemon actually exits — docker-stop style.
    // The daemon removes its pidfile on graceful shutdown, so
    // either an absent pidfile OR `kill -0 <pid>` returning false is
    // proof of exit. V0.4.6 bumps the wait to 35s so the daemon's
    // own 30s graceful timeout + abort-fallback path can complete
    // before we surface a warning. We never escalate to SIGKILL —
    // CLAUDE.md §三 "永不主动 kill 长 session" applies to ccteam's
    // own daemon too.
    let deadline = std::time::Instant::now() + Duration::from_secs(35);
    while std::time::Instant::now() < deadline {
        if !pidfile.exists() || !ccteam_core::daemon::pid_alive(pid) {
            println!("ccteam stop: gateway daemon exited.");
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
    // V0.6.5 F155 — `--check-codex-auto-critic` carries non-zero exit
    // codes (2 = codex unavailable, 3 = output malformed) so callers
    // (skill body, CI) can branch deterministically. The flag short-
    // circuits before the generic dispatch because it's the only
    // doctor mode that maps a structured outcome to a non-error exit
    // code (everything else is `Ok(())` or anyhow::bail!).
    if opts.check_codex_auto_critic {
        let (body, exit_code) = commands::run_check_codex_auto_critic();
        print!("{body}");
        if exit_code != 0 {
            std::process::exit(exit_code);
        }
        return Ok(());
    }
    // V0.6.6 F171 — `--verify-mcp` carries non-zero exit code (1) when
    // any STUB tool is registered so CI can gate the "0 STUB"
    // invariant. Same short-circuit pattern as --check-codex-auto-
    // critic: deterministic outcome → structured non-error exit.
    if opts.verify_mcp {
        let report = commands::run_verify_mcp();
        let body = if opts.verify_mcp_json {
            report.render_json()
        } else {
            report.render_text()
        };
        print!("{body}");
        if !report.ok() {
            std::process::exit(1);
        }
        return Ok(());
    }
    let body = commands::run_doctor(&paths, opts)?;
    print!("{body}");
    Ok(())
}

fn run_hook(cmd: HookCommand) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    // V0.6.1 F139: dispatch through the shared `ccteam_hooks::dispatch`
    // entry so the CLI fallback and the daemon HTTP route share one
    // codepath. `intercept-ask` returns a JSON decision that we echo to
    // stdout; the other hooks are side-effect-only.
    let (kind, action, needs_stdin): (&str, Option<&str>, bool) = match &cmd {
        HookCommand::ProgressAppend { event_type } => {
            ("progress-append", Some(event_type.as_str()), true)
        }
        HookCommand::LoadContext => ("load-context", None, true),
        HookCommand::InterceptAsk => ("intercept-ask", None, false),
        HookCommand::ChatProgress { event } => ("chat-progress", Some(event.as_str()), true),
    };
    let stdin = if needs_stdin {
        parse_hook_stdin_json()?
    } else {
        serde_json::Value::Null
    };
    let response = ccteam_hooks::dispatch(&paths, kind, action, &stdin)?;
    if let Some(value) = response {
        println!("{}", serde_json::to_string(&value)?);
    }
    Ok(())
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

/// V0.6.1 F130 — IM supervisor task knobs. Today the only operator
/// switch is `disabled` (mirror of `--no-imd`); future knobs (custom
/// credentials path, custom registry root) attach here without changing
/// `run_start`'s signature.
struct StartImdOpts {
    disabled: bool,
}

fn run_start(
    _tick_seconds: u64,
    _skip_tool_check: bool,
    _claude_argv_flag: Option<String>,
    web: StartWebOpts,
    imd: StartImdOpts,
) -> Result<()> {
    init_tracing();

    // V0.8 rmux — the mux backend default is rmux, resolved in the
    // library (`ccteam_harness::from_env` / `default_backend`): rmux is the
    // bundled always-available backend so ccteam works with no external
    // tmux. An operator opts out with `CCTEAM_MUX_BACKEND=tmux`. Log the
    // effective choice for ops visibility at orchestrator startup (no
    // set_var here — the library is the single source of truth).
    //
    // Validated end-to-end on Linux (rmux-smoke CI job: spawn→send→
    // capture→kill + reconnect-after-daemon-death). macOS/Windows run on
    // the CI matrix; real-claude mode-3 burn-in is the merge-to-main gate
    // (this is a no-merge evaluation branch). See
    // docs/versions/v0-8-rmux/w-flip-default-migration-plan.md.
    tracing::info!(
        backend = ?ccteam_harness::backend_kind_from_env(),
        "ccteam start: mux backend (set CCTEAM_MUX_BACKEND=tmux to use tmux)"
    );

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

    // Drain only clearly-stale triggers. The trigger path is per-user,
    // so integration tests can have several daemons alive at once; a
    // daemon starting after another test wrote a fresh trigger must not
    // erase that fresh shutdown request before the target sees it.
    let trigger = shutdown_trigger_path();
    let stale_trigger = std::fs::metadata(&trigger)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|mtime| std::time::SystemTime::now().duration_since(mtime).ok())
        .is_some_and(|age| age > Duration::from_secs(2));
    if stale_trigger {
        let _ = std::fs::remove_file(&trigger);
    }

    // v8.1: no-slug `ccteam start` is the resident gateway daemon, not
    // the flow/orchestrator loop. The legacy tick/claude-argv flags are
    // accepted by clap for compatibility but are intentionally ignored
    // here; agent process settings live in the harness adapters.
    let pidfile = ccteam_core::write_pidfile(&paths)?;
    tracing::info!(pidfile = %pidfile.display(), "ccteam gateway pidfile written");

    // Print a single banner up front so the operator can paste the web
    // URL into a browser without grepping mid-log noise. Skip when
    // web is disabled. Token is resolved here (read or pre-generate)
    // only when bind is non-loopback so the loopback fast-path stays
    // zero-IO.
    if !web.disabled {
        print_web_banner(&paths, &web);
    }

    // We need the paths for final pidfile cleanup after the async
    // runtime has drained the gateway tasks.
    let cleanup_paths = paths.clone();
    let hook_sink_paths = paths.clone();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")?;
    let result = runtime.block_on(async move {
        // v8.1 gateway + web + hook sink share one shutdown signal
        // (Ctrl-C, SIGTERM, or `ccteam stop` trigger file). No flow
        // orchestrator is constructed in this path.
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let web_chat_bridge = if !web.disabled && !imd.disabled {
            Some(web_chat_bridge::build())
        } else {
            None
        };

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
            let web_bridge = web_chat_bridge
                .as_ref()
                .map(|bridge| {
                    (
                        bridge.inbound_tx.clone(),
                        bridge.outbound_tx.clone(),
                        bridge.backlog.clone(),
                        bridge.conns.clone(),
                    )
                });
            Some(tokio::spawn(async move {
                ccteam_web::serve_with_state_factory_and_shutdown(
                    opts,
                    move |paths, auth| {
                        let state = ccteam_web::AppState::with_auth(paths, auth);
                        if let Some((inbound, outbound, backlog, conns)) = web_bridge {
                            state.with_chat_bridge(inbound, outbound, backlog, conns)
                        } else {
                            state
                        }
                    },
                    async move {
                        let _ = rx.changed().await;
                    },
                )
                .await
            }))
        };

        // V0.8.4 P2b — shared gateway-event channel: the IM daemon
        // consumes it; the mcp.sock handler clones the sender so
        // `chat_send_file` reuses the same outbound funnel. Only created
        // when IM is enabled (no consumer ⇒ nothing to deliver to).
        let (gw_event_tx, gw_event_rx) = if imd.disabled {
            (None, None)
        } else {
            let (t, r) =
                tokio::sync::mpsc::unbounded_channel::<ccteam_im::gateway::GatewayEvent>();
            (Some(t), Some(r))
        };

        let imd_handle = if imd.disabled {
            tracing::info!("ccteam start: --no-imd set; IM gateway task skipped");
            None
        } else {
            let mut rx = shutdown_rx.clone();
            let mut args = ccteam_im::DaemonArgs {
                gateway_event_tx: gw_event_tx.clone(),
                gateway_event_rx: gw_event_rx,
                ..Default::default()
            };
            if let Some(bridge) = web_chat_bridge.as_ref() {
                let mut channels = ccteam_im::daemon::ChannelMap::new();
                channels.insert("web".to_string(), bridge.channel.clone());
                args.extra_channels = Some(channels);
            }
            Some(tokio::spawn(async move {
                ccteam_im::run_daemon_with_shutdown(args, async move {
                    let _ = rx.changed().await;
                })
                .await
            }))
        };

        let mut rx = shutdown_rx.clone();
        let mcp_sink = gw_event_tx.clone();
        let mcp_handle = tokio::spawn(async move {
            serve_mcp_socket(paths, mcp_sink, async move {
                let _ = rx.changed().await;
            })
            .await
        });

            // V0.8 rmux W6 — flag-gated hook-sink listener (Option C).
            // ONLY when `CCTEAM_HOOK_VIA_DAEMON=1`: bind the ccteam-owned
            // `~/.ccteam/run/hook.sock`, consume each forwarded HookEvent,
            // and translate it to the same progress.jsonl line the legacy
            // `ccteam internal hook <kind> <action>` path would have
            // written — making the orchestrator process the SINGLE writer
            // and closing the two-writer race. When the flag is unset
            // (the default) we do NOT bind the sink: nothing changes, the
            // hook subprocess keeps writing progress.jsonl directly via
            // the `hook.sh chat-progress <arg>` path.
            //
            // The consumer awaits each dispatch (on the blocking pool, so
            // the file IO doesn't stall the runtime) before the next
            // recv, preserving append order — the single-writer invariant
            // the whole reroute exists to provide.
        let hook_sink_handle = if ccteam_core::hooks_dispatcher::hook_via_daemon_enabled() {
            let socket = ccteam_harness::default_ccteam_hook_socket_path();
            match ccteam_harness::HookSink::bind(&socket) {
                Ok(mut sink) => {
                    tracing::info!(
                        socket = %socket.display(),
                        "W6 hook-sink bound (CCTEAM_HOOK_VIA_DAEMON=1): daemon is single progress.jsonl writer"
                    );
                    let dispatch_paths = hook_sink_paths.clone();
                    let mut rx = shutdown_rx.clone();
                    Some(tokio::spawn(async move {
                        loop {
                            tokio::select! {
                                _ = rx.changed() => break,
                                maybe = sink.recv() => {
                                    let Some(event) = maybe else { break };
                                    ccteam_harness::execution::typed_events::enrich_session_from_hook(&event);
                                    let dispatch_paths = dispatch_paths.clone();
                                    let res = tokio::task::spawn_blocking(move || {
                                        let stdin: serde_json::Value =
                                            serde_json::from_str(&event.payload_json)
                                                .unwrap_or(serde_json::Value::Null);
                                        ccteam_hooks::dispatch(
                                            &dispatch_paths,
                                            &event.kind,
                                            event.action.as_deref(),
                                            &stdin,
                                        )
                                    })
                                    .await;
                                    match res {
                                        Ok(Ok(_)) => {}
                                        Ok(Err(err)) => tracing::warn!(
                                            error = %err,
                                            "W6 hook-sink: dispatch failed; event dropped"
                                        ),
                                        Err(je) => tracing::warn!(
                                            ?je,
                                            "W6 hook-sink: dispatch task panicked"
                                        ),
                                    }
                                }
                            }
                        }
                        drop(sink);
                    }))
                }
                Err(err) => {
                    tracing::warn!(
                        socket = %socket.display(),
                        error = %err,
                        "W6 hook-sink bind failed; chat-mode hooks will fail to route (set CCTEAM_HOOK_VIA_DAEMON only when intended)"
                    );
                    None
                }
            }
        } else {
            None
        };

        let mut gateway_shutdown = shutdown_rx.clone();
        let _ = gateway_shutdown.changed().await;

        const TASK_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

        if let Some(h) = web_handle {
            match tokio::time::timeout(TASK_DRAIN_TIMEOUT, h).await {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(err))) => tracing::warn!(?err, "ccteam web exited with error"),
                Ok(Err(je)) if je.is_cancelled() => {}
                Ok(Err(je)) => tracing::warn!(?je, "ccteam web task panicked"),
                Err(_) => {
                    tracing::warn!(
                        timeout_secs = TASK_DRAIN_TIMEOUT.as_secs(),
                        "ccteam web drain timed out; aborting (port will be released by OS)"
                    );
                }
            }
        }
        if let Some(h) = imd_handle {
            match tokio::time::timeout(TASK_DRAIN_TIMEOUT, h).await {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(err))) => tracing::warn!(?err, "ccteam-im exited with error"),
                Ok(Err(je)) if je.is_cancelled() => {}
                Ok(Err(je)) => tracing::warn!(?je, "ccteam-im task panicked"),
                Err(_) => {
                    tracing::warn!(
                        timeout_secs = TASK_DRAIN_TIMEOUT.as_secs(),
                        "ccteam-im drain timed out; aborting"
                    );
                }
            }
        }
        match tokio::time::timeout(TASK_DRAIN_TIMEOUT, mcp_handle).await {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(err))) => tracing::warn!(?err, "ccteam MCP socket exited with error"),
            Ok(Err(je)) if je.is_cancelled() => {}
            Ok(Err(je)) => tracing::warn!(?je, "ccteam MCP socket task panicked"),
            Err(_) => {
                tracing::warn!(
                    timeout_secs = TASK_DRAIN_TIMEOUT.as_secs(),
                    "ccteam MCP socket drain timed out; aborting"
                );
            }
        }
        if let Some(h) = hook_sink_handle {
            match tokio::time::timeout(TASK_DRAIN_TIMEOUT, h).await {
                Ok(Ok(())) => {}
                Ok(Err(je)) if je.is_cancelled() => {}
                Ok(Err(je)) => tracing::warn!(?je, "W6 hook-sink task panicked"),
                Err(_) => {
                    tracing::warn!(
                        timeout_secs = TASK_DRAIN_TIMEOUT.as_secs(),
                        "W6 hook-sink drain timed out; aborting"
                    );
                }
            }
        }
        signal_task.abort();

        tracing::info!(
            "graceful shutdown complete; agent sessions (if any) left running intentionally — \
             `ccteam start` will reattach to them"
        );

        Ok(())
    });
    // Keep runtime teardown bounded even if a blocking hook dispatch is
    // mid-flight during shutdown.
    runtime.shutdown_timeout(Duration::from_secs(5));
    ccteam_core::remove_pidfile(&cleanup_paths);
    result
}

#[cfg(unix)]
async fn serve_mcp_socket<F>(
    paths: CcteamPaths,
    sink: Option<GatewayEventSink>,
    shutdown: F,
) -> Result<()>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let socket = ccteam_core::daemon_socket_path(&paths);
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create MCP socket dir {}", parent.display()))?;
    }
    let _ = std::fs::remove_file(&socket);
    let listener = tokio::net::UnixListener::bind(&socket)
        .with_context(|| format!("bind MCP socket {}", socket.display()))?;
    tracing::info!(socket = %socket.display(), "ccteam MCP socket listening");

    let mut shutdown = Box::pin(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => {
                let _ = std::fs::remove_file(&socket);
                return Ok(());
            }
            accepted = listener.accept() => {
                let (stream, _addr) = accepted
                    .with_context(|| format!("accept MCP socket {}", socket.display()))?;
                let paths = paths.clone();
                let sink = sink.clone();
                tokio::spawn(async move {
                    if let Err(err) = handle_mcp_socket_connection(paths, sink, stream).await {
                        tracing::warn!(error = %err, "MCP socket connection failed");
                    }
                });
            }
        }
    }
}

#[cfg(not(unix))]
async fn serve_mcp_socket<F>(
    _paths: CcteamPaths,
    _sink: Option<GatewayEventSink>,
    shutdown: F,
) -> Result<()>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    shutdown.await;
    Ok(())
}

#[cfg(unix)]
async fn handle_mcp_socket_connection(
    paths: CcteamPaths,
    sink: Option<GatewayEventSink>,
    stream: tokio::net::UnixStream,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = tokio::io::BufReader::new(reader).lines();
    while let Some(line) = lines.next_line().await.context("read MCP socket line")? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let req: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(value) => value,
            Err(err) => {
                let response = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": { "code": -32700, "message": format!("parse error: {err}") },
                });
                let mut out = serde_json::to_string(&response)?;
                out.push('\n');
                writer.write_all(out.as_bytes()).await?;
                writer.flush().await?;
                continue;
            }
        };
        // V0.8.4 P2b — intercept the live `chat_send_file` tool here (we
        // own the gateway-event sink); everything else routes to the
        // stateless handler. Intercepting BEFORE handle_request keeps it
        // from looping back into the stdio forward branch.
        let response = if is_chat_send_file_call(&req) {
            Some(execute_chat_send_file(&req, sink.as_ref()).await)
        } else {
            mcp_serve::handle_request(&paths, &req).await
        };
        if let Some(response) = response {
            let mut out = serde_json::to_string(&response)?;
            out.push('\n');
            writer.write_all(out.as_bytes()).await?;
            writer.flush().await?;
        }
    }
    Ok(())
}

/// V0.8.4 P2b — sender half of the gateway-event channel that the IM
/// daemon consumes; `chat_send_file` clones it to reuse that outbound
/// funnel (P0 split + durable ledger + failure echo).
type GatewayEventSink = tokio::sync::mpsc::UnboundedSender<ccteam_im::gateway::GatewayEvent>;

/// Monotonic id source so each `chat_send_file` gets a distinct durable
/// ledger row (avoids `{id}-0` collisions in `outbound.jsonl`).
static CHAT_SEND_FILE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn is_chat_send_file_call(req: &serde_json::Value) -> bool {
    req.get("method").and_then(|m| m.as_str()) == Some("tools/call")
        && req.pointer("/params/name").and_then(|n| n.as_str()) == Some("ccteam__chat_send_file")
}

/// Resolve addressing, validate the file, and enqueue a `GatewayEvent`
/// onto the shared sink (the IM consumer does the actual `sendPhoto` /
/// `sendDocument`). Returns a tools/call-shaped JSON-RPC response.
async fn execute_chat_send_file(
    req: &serde_json::Value,
    sink: Option<&GatewayEventSink>,
) -> serde_json::Value {
    let id = req.get("id").cloned().unwrap_or(serde_json::Value::Null);
    let args = req
        .pointer("/params/arguments")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let (text, is_error) = match run_chat_send_file(&args, sink) {
        Ok(text) => (text, false),
        Err(text) => (text, true),
    };
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "content": [{ "type": "text", "text": text }],
            "isError": is_error,
        },
    })
}

fn run_chat_send_file(
    args: &serde_json::Value,
    sink: Option<&GatewayEventSink>,
) -> std::result::Result<String, String> {
    let sink = sink.ok_or_else(|| "chat_send_file: IM gateway not running".to_string())?;
    let bots =
        ccteam_im::list_bots().map_err(|e| format!("chat_send_file: registry error: {e}"))?;
    let seq = CHAT_SEND_FILE_SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let event = build_send_file_event(args, &bots, seq)?;
    let dest = format!("{}/{}", event.channel, event.chat_id);
    sink.send(event)
        .map_err(|_| "chat_send_file: gateway sink closed".to_string())?;
    Ok(format!("delivered: queued to {dest}"))
}

/// Telegram bot-send ceilings: `sendPhoto` ≤ 10 MB, `sendDocument` ≤ 50 MB.
const OUTBOUND_PHOTO_MAX_BYTES: u64 = 10 * 1024 * 1024;
const OUTBOUND_DOCUMENT_MAX_BYTES: u64 = 50 * 1024 * 1024;

/// Pure core of `run_chat_send_file`: parse args, validate the file
/// (exists + within the send ceiling), resolve the home chat from
/// `bots`, and build the `GatewayEvent`. Only I/O is the file
/// stat, so it is unit-testable without a live registry or sink.
fn build_send_file_event(
    args: &serde_json::Value,
    bots: &[ccteam_im::BotRegistration],
    seq: u64,
) -> std::result::Result<ccteam_im::gateway::GatewayEvent, String> {
    use ccteam_im::transport::OutboundFileKind;
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "chat_send_file: missing `path`".to_string())?;
    let caption = args
        .get("caption")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let slug = args
        .get("slug")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let role = args
        .get("role")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let kind = parse_outbound_kind(args.get("kind").and_then(|v| v.as_str()), path);

    let meta =
        std::fs::metadata(path).map_err(|_| format!("chat_send_file: file not found: {path}"))?;
    let max = match kind {
        OutboundFileKind::Photo => OUTBOUND_PHOTO_MAX_BYTES,
        OutboundFileKind::Document => OUTBOUND_DOCUMENT_MAX_BYTES,
    };
    if meta.len() > max {
        return Err(format!(
            "chat_send_file: file too large ({} MB) for {:?} (limit {} MB)",
            meta.len() / (1024 * 1024),
            kind,
            max / (1024 * 1024),
        ));
    }
    let (channel, chat_id) = ccteam_im::resolve_home_chat(slug, role, bots)
        .ok_or_else(|| format!("chat_send_file: no registered chat for {slug}/{role}"))?;
    Ok(ccteam_im::gateway::GatewayEvent {
        id: format!("chat-send-file-{slug}-{role}-{seq}"),
        channel,
        chat_id,
        thread_ts: None,
        content: String::new(),
        kind: ccteam_im::gateway::GatewayEventKind::Answer,
        attachments: vec![ccteam_im::transport::OutboundFile {
            path: path.to_string(),
            caption,
            kind,
        }],
    })
}

/// `kind` arg → [`OutboundFileKind`], inferring photo from common image
/// extensions when omitted.
fn parse_outbound_kind(kind: Option<&str>, path: &str) -> ccteam_im::transport::OutboundFileKind {
    use ccteam_im::transport::OutboundFileKind;
    match kind {
        Some("photo") => OutboundFileKind::Photo,
        Some("document") => OutboundFileKind::Document,
        _ => {
            let lower = path.to_lowercase();
            let is_image = [".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp"]
                .iter()
                .any(|ext| lower.ends_with(ext));
            if is_image {
                OutboundFileKind::Photo
            } else {
                OutboundFileKind::Document
            }
        }
    }
}

async fn wait_for_shutdown_signal() {
    // V0.4.6 F86 — `ccteam stop` writes `/tmp/ccteam-<user>.shutdown`
    // instead of sending SIGTERM. The daemon polls for the file and
    // collapses to the same shutdown path as SIGTERM (orchestrator
    // graceful cancel via Notify channel). SIGTERM is retained for
    // systemd / docker-stop callers; either trigger is sufficient.
    let trigger = shutdown_trigger_path();
    let self_pid = std::process::id();
    let trigger_poll = async {
        let mut ticker = tokio::time::interval(Duration::from_millis(250));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            if trigger.exists() {
                let targeted_pid = std::fs::read_to_string(&trigger)
                    .ok()
                    .and_then(|s| s.trim().parse::<u32>().ok());
                if targeted_pid.is_some_and(|pid| pid != self_pid) {
                    continue;
                }
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
        // Classify each project's silence into a short verdict so the
        // operator reads "STUCK" instead of decoding raw seconds. Reuses
        // the same `commands::stall_level` tiers the `--json` path emits,
        // keeping the human + machine views consistent.
        let mut needs_attention: Vec<(&str, String)> = Vec::new();
        for p in &projects {
            let age = humanize_secs(p.age_seconds);
            let silent = humanize_secs(p.stall_silent_seconds);
            let verdict = commands::stall_verdict(p.stall_silent_seconds);
            if verdict != "OK" {
                needs_attention.push((p.state.slug.as_str(), silent.clone()));
            }
            println!(
                "    {:<32}  age {:>8}  last-event {:>8}  {}",
                p.state.slug, age, silent, verdict
            );
        }
        // One actionable hint line per warn-or-higher project so the
        // operator knows the exact peek → attach takeover sequence.
        for (slug, silent) in &needs_attention {
            println!("    {}", commands::stall_takeover_hint(slug, silent));
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
    let spec = ccteam_flow::workflow::WorkflowSpec::load_for_project(&project_dir)
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

fn run_internal_peek(slug: &str, role: Option<&str>) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    let body = commands::run_peek_with_role(&paths, slug, role)?;
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

/// V0.6.5 F165 — tracing init for the stdio MCP server.
///
/// `ccteam mcp-serve` speaks line-delimited JSON-RPC over stdin/stdout
/// (`docs/interfaces.md` §12). The default [`init_tracing`] writer is
/// stdout, which collides with the JSON-RPC frame channel — the very
/// first `tools/list` reply can be preceded by a `tracing::info!` log
/// line, breaking strict JSON-per-line parsers. Pin the fmt layer to
/// stderr for this subcommand so stdout is reserved for protocol
/// frames; operators still see tracing output via `2>` redirection or
/// the controlling terminal.
///
/// Scope: stdio MCP server ONLY. Daemon mode (`ccteam start`) and the
/// web surface (`ccteam web`) keep stdout writes because their stdout
/// is the human / journalctl readout, not a wire protocol.
fn init_tracing_stderr() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,ccteam_core=info")),
        )
        .with_writer(std::io::stderr)
        .try_init();
}

#[cfg(test)]
mod chat_send_file_tests {
    use super::*;
    use ccteam_im::transport::OutboundFileKind;

    fn bot(slug: &str, role: &str, platform: &str, chat: &str) -> ccteam_im::BotRegistration {
        ccteam_im::BotRegistration {
            workflow_slug: slug.into(),
            role: role.into(),
            vendor: ccteam_harness::AgentVendor::Claude,
            persona_id: None,
            im_platform: platform.into(),
            im_chat_id: chat.into(),
            chat_handle: None,
            project_dir: None,
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn parse_outbound_kind_infers_photo_from_extension() {
        assert_eq!(
            parse_outbound_kind(None, "/x/shot.PNG"),
            OutboundFileKind::Photo
        );
        assert_eq!(
            parse_outbound_kind(None, "/x/a.jpeg"),
            OutboundFileKind::Photo
        );
        assert_eq!(
            parse_outbound_kind(None, "/x/report.pdf"),
            OutboundFileKind::Document
        );
        // Explicit kind overrides the extension.
        assert_eq!(
            parse_outbound_kind(Some("document"), "/x/shot.png"),
            OutboundFileKind::Document
        );
    }

    #[test]
    fn build_send_file_event_resolves_home_chat_and_attaches() {
        let tmp = tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("shot.png");
        std::fs::write(&file, b"png").unwrap();
        let bots = vec![bot("dev-foo", "lead", "telegram", "chat-42")];
        let args = serde_json::json!({
            "path": file.to_string_lossy(),
            "caption": "the chart",
            "slug": "dev-foo",
            "role": "lead",
        });
        let evt = build_send_file_event(&args, &bots, 7).unwrap();
        assert_eq!(evt.channel, "telegram");
        assert_eq!(evt.chat_id, "chat-42");
        assert_eq!(evt.attachments.len(), 1);
        assert_eq!(evt.attachments[0].kind, OutboundFileKind::Photo);
        assert_eq!(evt.attachments[0].caption.as_deref(), Some("the chart"));
        assert!(evt.id.ends_with("-7"));
    }

    #[test]
    fn build_send_file_event_errors_on_missing_file() {
        let bots = vec![bot("dev-foo", "lead", "telegram", "chat-42")];
        let args = serde_json::json!({
            "path": "/nope/does-not-exist.png", "slug": "dev-foo", "role": "lead",
        });
        let err = build_send_file_event(&args, &bots, 0).unwrap_err();
        assert!(err.contains("file not found"), "got: {err}");
    }

    #[test]
    fn build_send_file_event_errors_on_unregistered_chat() {
        let tmp = tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("x.txt");
        std::fs::write(&file, b"hi").unwrap();
        let bots = vec![bot("dev-foo", "lead", "telegram", "chat-42")];
        let args = serde_json::json!({
            "path": file.to_string_lossy(), "slug": "dev-foo", "role": "ghost",
        });
        let err = build_send_file_event(&args, &bots, 0).unwrap_err();
        assert!(err.contains("no registered chat"), "got: {err}");
    }

    #[test]
    fn build_send_file_event_errors_on_oversized_photo() {
        let tmp = tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("huge.png");
        let f = std::fs::File::create(&file).unwrap();
        f.set_len(11 * 1024 * 1024).unwrap(); // 11 MB (sparse) > 10 MB photo limit
        let bots = vec![bot("dev-foo", "lead", "telegram", "chat-42")];
        let args = serde_json::json!({
            "path": file.to_string_lossy(), "slug": "dev-foo", "role": "lead",
        });
        let err = build_send_file_event(&args, &bots, 0).unwrap_err();
        assert!(err.contains("too large"), "got: {err}");
    }
}
