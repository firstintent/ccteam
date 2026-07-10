//! `ccteam` binary entry point.

mod clipboard;
mod commands;
mod mcp_serve;
// ToolGroup enum + CCTEAM_DISABLE_TOOLS filter + chat send_file schema.
mod mcp_chat_tools;
// v0.8.7 W1 — `ccteam__session_*` tools (cto scheduling). Stdio side
// forwards to the daemon over mcp.sock; the daemon-side handler
// (`ccteam_im::mcp::McpDispatch`) holds the gateway + enforces the cto gate.
mod mcp_session_tools;
mod mcp_tool_groups;
mod web_chat_bridge;
// v0.8.22 — bare `ccteam doctor` full readiness checkup (claude/codex/
// tmux binaries, MCP registration, daemon health, pricing, home-layout
// drift). Historical one-shot migration/repair flags keep dispatching
// through `commands::run_doctor` unchanged; see `doctor::is_bare_invocation`.
mod doctor;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

use ccteam_core::CcteamPaths;
use commands::{InitOptions, OutputFormat};

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
    /// v0.8.20 — override the ccteam home dir (default `~/.ccteam`). Lets a
    /// second, fully isolated instance live under e.g. `~/.ccteam2`. Equivalent
    /// to setting `CCTEAM_HOME`; this flag wins when both are given.
    #[arg(long, value_name = "DIR", global = true)]
    home: Option<PathBuf>,
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
    /// global installs (MCP, skill), or `-y` / `--yes`
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
        /// use `ccteam project new <slug>` (e.g. `ccteam project new demo`).
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
        /// v0.8.20 F1: set the project owner identity (`ProjectState.owner`,
        /// `"channel:chat_id"` — e.g. `user:<tenant>`). A bare value (no `:`)
        /// is scoped to the per-user identity namespace (`alice` → `user:alice`). Present
        /// overrides an existing owner on re-init (no `--force` needed);
        /// absent preserves it.
        #[arg(long, value_name = "OWNER")]
        owner: Option<String>,
    },
    /// Run the v8.1 gateway daemon (IM gateway plus, by default, the
    /// web UI in the same process). Foreground is the only supported
    /// mode — `ccteam start` is enough; the `--foreground` flag is
    /// accepted for back-compat but no longer required.
    /// Pass `--no-web` to run the gateway without web.
    Start {
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
    },
    /// V0.4.1: one-screen aggregate health view. Reports daemon
    /// heartbeat age, every project's slug + age + last-event time with
    /// its tracked sessions (role · vendor · status · sid) nested
    /// underneath, and the embedded-web token + LAN URL. Replaces having
    /// to grep `ls` + `session ls` + multiple `doctor` checks.
    Status,
    /// Internal commands — hook handlers + MCP integration
    /// points + low-level utilities (mux hook-emit, probe-project, web).
    /// Not user-facing day to day; the `ccteam-control`
    /// skill drive these. Hidden from top-level help; run
    /// `ccteam internal --help` for the list.
    #[command(hide = true)]
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
    ///     bg job + teammates alive (re-attach later via
    ///     `claude attach <lead_id>`).
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
    /// Project lifecycle group: `ccteam project <ls|show|new|stop|rm>`.
    ///
    /// `ls`/`show` inspect registered projects; `new` scaffolds a fresh
    /// project under `<projects_root>/<team>-<slug>/`; `stop` halts a
    /// project's live sessions without removing it; `rm` un-registers a
    /// project and (with `--purge`) deletes ccteam's on-disk footprint.
    /// Run `ccteam project --help` for the list.
    Project {
        #[command(subcommand)]
        cmd: ProjectCommand,
    },
    /// Session group: `ccteam session
    /// <ls|attach|pause|resume|register|unregister|persona|add-tool|role>`.
    ///
    /// `ls` enumerates live gateway chat sessions; `attach` reaches a
    /// chat-mode bot session (or the project session); `pause`/`resume`
    /// gate a project's auto-dispatch; `register`/`unregister` manage the
    /// chat-mode bot registry; `persona`/`add-tool` edit a bot's
    /// `.claude/agents/<bot>.md`; `role` switches a chat session's role.
    /// Run `ccteam session --help` for the list.
    Session {
        #[command(subcommand)]
        cmd: SessionCommand,
    },
    /// Role / plugin marketplace group: `ccteam role <search|add|list>`.
    ///
    /// Browse the curated ccteam-hub marketplace (`search`), one-shot install
    /// a plugin into the project's `.claude/` (`add <id>`, fetches + sha256-
    /// verifies the body over HTTPS), or list the roles already installed in a
    /// project (`list`, wraps the resource-API reader). This is a different
    /// noun from `session role` (which switches a *live* chat session's role
    /// inside the daemon).
    Role {
        #[command(subcommand)]
        cmd: RoleCommand,
    },
    /// Bare `ccteam doctor` (no flags) runs a full readiness checkup:
    /// claude/codex binaries, tmux, MCP registration (Claude + Codex),
    /// daemon health, pricing staleness, and `~/.ccteam` home-layout
    /// drift — one `[PASS]/[WARN]/[FAIL]/[SKIP]` line per check plus a
    /// summary line, exit code 1 iff any check FAILs. `--verify-mcp` is
    /// a separate CI-oriented invariant (the MCP tool-surface / STUB
    /// self-check) and stays a visible flag. A handful of historical
    /// one-shot migration / repair flags from older ccteam versions
    /// still work exactly as before but are hidden from `--help` to
    /// keep it short (see `docs/versions/` for what each one does).
    Doctor {
        /// Print + return what would happen without touching the
        /// filesystem. Only meaningful paired with one of the hidden
        /// migration/repair flags below.
        #[arg(long, default_value_t = false, hide = true)]
        dry_run: bool,
        /// Cross-check every shipped phase template's tools_required
        /// against the live tool surface (plugin pipeline + user
        /// agents/skills + MCP servers) and print a markdown report.
        #[arg(long, default_value_t = false, hide = true)]
        tool_surface: bool,
        // v0.8.6 Item 4 — the setup actions formerly exposed here
        // (`--install-mcp` / `--install-skill` / `--install-all`) moved to
        // `ccteam config`. `doctor` is now
        // diagnostics / self-check / repair only.
        /// M4.2: write `~/.claude/rules/ccteam-lessons-<team>.md`
        /// with `<!-- ccteam-managed:lessons begin/end -->` markers + `paths:`
        /// frontmatter scope. Idempotent — re-runs no-op when markers are
        /// intact, repair (single canonical block at end-of-file) when not.
        /// User content outside markers is preserved. Discovers teams by
        /// scanning `~/.ccteam/teams/<name>/team.yaml` for non-empty
        /// `retro_schema` (V0.2 M0.16.2).
        #[arg(long, default_value_t = false, hide = true)]
        install_memory_bridge: bool,
        /// V0.2 M0.16.2: re-write every shipped team's seed
        /// (`~/.ccteam/teams/<name>/team.yaml` + `~/.ccteam/<phase_dir>/*.md`)
        /// from the in-binary bundle. `force=false` preserves operator
        /// hand-edits; pair with `--force` to clobber. Useful after a
        /// ccteam upgrade ships schema-additive team.yaml fields.
        #[arg(long, default_value_t = false, hide = true)]
        reset_shipped_teams: bool,
        /// V0.2 M0.18.5: load + validate the named team's
        /// `team.yaml` and every phase markdown under its phase dir.
        /// Fails-loud on schema violations and IO-contract gaps; warns
        /// (without failing) on protocol-literal residue in phase
        /// bodies. Pass the team name as the value (e.g.
        /// `--validate-team dev`).
        #[arg(long, value_name = "TEAM", hide = true)]
        validate_team: Option<String>,
        /// V0.2 M0.20: remove stale `~/.claude/agents/<name>.md`
        /// symlinks left by the V0.1 `--install-recommended-agents`
        /// path. Spawned project sessions now resolve plugin agents
        /// through Claude Code's in-memory plugin pipeline via
        /// `enabledPlugins` in `<project>/.claude/settings.json`, so
        /// these symlinks are obsolete. Idempotent — no-op when no
        /// marketplace symlinks remain.
        #[arg(long, default_value_t = false, hide = true)]
        migrate_recommended_agents: bool,
        /// V0.2.2 F38: render a one-shot PNG screenshot of the given
        /// project's tmux pane to verify the `vt100 + imageproc` path
        /// end-to-end. Reports the font in use + the resulting PNG
        /// path or the degrade reason (tmux missing / font / IO).
        #[arg(long, value_name = "SLUG", hide = true)]
        screenshot_smoke: Option<String>,
        /// V0.4.2 F74: fold V0.4.1 project layout into the new
        /// `~/.ccteam/config.yaml`. Walks `~/projects/*` and appends
        /// every parseable `.ccteam/state.json` to `config.yaml::
        /// projects[]`; folds `~/.ccteam/watchdog.yaml` into
        /// `config.yaml::watchdog` and renames the old file to
        /// `watchdog.yaml.migrated`. Idempotent.
        #[arg(long, default_value_t = false, hide = true)]
        migrate_v041_to_v042: bool,
        /// V0.4.6 F83: move every registered project's root
        /// `workflow.yaml` into `<project>/.ccteam/workflow.yaml`.
        /// Default is dry-run; pair with `--apply` to actually move
        /// the files. Conflicts (both locations populated) are
        /// fail-safe — neither file is touched and the user is told
        /// to resolve by hand. Idempotent.
        #[arg(long, default_value_t = false, hide = true)]
        migrate_workflow_to_ccteam_dir: bool,
        /// V0.4.6 F85: reclaim terminated `~/.claude/jobs/<id>/`
        /// directories older than
        /// `~/.ccteam/config.yaml::claude_jobs_retention_days` (default
        /// 7 days). Default is dry-run — prints what would be removed
        /// without touching disk. Pair with `--apply` to actually
        /// `rm -rf` eligible entries. Never touches dirs whose
        /// `state.json::state == "working"` or whose `state.json` is
        /// missing / unparseable.
        #[arg(long, default_value_t = false, hide = true)]
        gc_claude_jobs: bool,
        /// V0.4.6 F91: walk every registered project's
        /// `.claude/settings.json` and strip the legacy
        /// `ccteam hook cost-accumulate` PostToolUse entry. Idempotent —
        /// re-runs after success are no-ops. Pair with `--dry-run` to
        /// preview the scrub.
        #[arg(long, default_value_t = false, hide = true)]
        update_hooks: bool,
        /// V0.5.0 F92: print the embedded `pricing.json` schema_version
        /// next to today's date and WARN when the table is older than
        /// 180 days. No fs mutation; pure readout to remind operators
        /// to upgrade ccteam when the bundled rate sheet ages out.
        #[arg(long, default_value_t = false, hide = true)]
        check_pricing_version: bool,
        /// V0.6.0 Wave 3 F112: probe `codex --version` and warn when
        /// older than 0.131 (minimum supported by Wave 3 mode-3 codex
        /// bot path). No fs mutation. Pairs with --check-codex-auth.
        #[arg(long, default_value_t = false, hide = true)]
        check_codex_version: bool,
        /// V0.6.0 Wave 3 F112: probe `codex login status` and report
        /// whether the operator is logged in to ChatGPT / API. No fs
        /// mutation. Pairs with --check-codex-version.
        #[arg(long, default_value_t = false, hide = true)]
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
        #[arg(long, default_value_t = false, hide = true)]
        check_codex_auto_critic: bool,
        /// V0.6.6 F173: reconcile `<ccteam_root>/cost-budget.json` ledger
        /// rows against every registered project's `progress.jsonl`
        /// over the last 24h. Reports "cost orphan: N <vendor> agent_done
        /// events in progress.jsonl, M rows in ledger" for any mismatch
        /// (vendor adapter call recorded a `progress.jsonl` event but no
        /// ledger row). Silent OK when fully reconciled. No fs mutation —
        /// pure-readout invariant check; future regressions (a new vendor
        /// adapter that forgets the ledger hook) surface here.
        #[arg(long, default_value_t = false, hide = true)]
        check_cost_orphan: bool,
        /// V0.6.1 F139: materialize the `~/.ccteam/hooks/hook.sh`
        /// daemon-aware Claude Code hook dispatcher (idempotent, chmod
        /// 0755). Run after a ccteam binary upgrade to refresh the
        /// script body. `ccteam init` already does this on first
        /// install.
        #[arg(long, default_value_t = false, hide = true)]
        install_hooks: bool,
        /// V0.6.1 F139: rewrite every registered project's
        /// `.claude/settings.json` so hook commands invoke
        /// `~/.ccteam/hooks/hook.sh` instead of the V0.4.6 / V0.6.0
        /// `<ccteam-bin> internal hook ...` form (or older `cct hook
        /// ...` / `ccteam hook ...` forms). Idempotent; pair with
        /// `--dry-run` to preview without writing.
        #[arg(long, default_value_t = false, hide = true)]
        migrate_hook_commands: bool,
        /// V0.4.6 F83/F85: pair with `--migrate-workflow-to-ccteam-dir`
        /// or `--gc-claude-jobs` to commit changes to disk instead of
        /// previewing them. Without it, those subcommands run as
        /// dry-run.
        #[arg(long, default_value_t = false, hide = true)]
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
    /// v0.8.6 Item 4 — setup hub. Bare `ccteam config` opens an
    /// interactive menu (register/refresh the ccteam MCP server, set the
    /// IM Telegram token, show preferences). The non-interactive forms are
    /// the headless/CI surface and wrap the preferences store
    /// (`~/.ccteam/preferences.toml`):
    ///
    ///   - `ccteam config show`          print the active preferences
    ///   - `ccteam config get <key>`     read one preference
    ///   - `ccteam config <key> <value>` set one preference
    ///
    /// `config` absorbs the former `ccteam prefs` command plus the setup
    /// actions retired from `ccteam doctor` (`--install-mcp`) and the
    /// `ccteam-im-setup` skill (IM token onboarding).
    Config {
        /// Positional words. Empty → interactive menu. `show` → print
        /// preferences. `get <key>` → read one preference. `<key> <value>`
        /// → set one preference (the bare two-arg form). A single
        /// non-keyword word is treated as `get <key>`.
        #[arg(value_name = "ARGS", num_args = 0..=2)]
        args: Vec<String>,
    },
}

/// v0.8.6 W4a — `ccteam session` subcommand surface. Folds the former
/// flat `sessions` / `attach` / `pause` / `resume` commands and the
/// `admin` chat-mode bot ops (register / unregister / persona / add-tool
/// / list) into one group. The bot ops still mirror the
/// `mcp__ccteam__admin_*` / `mcp__ccteam__chat_*` MCP tools.
#[derive(Subcommand)]
enum SessionCommand {
    /// List live gateway chat-mode bot sessions (`ccteam-chat-<slug>-<sid>`).
    ///
    /// Read-only control-plane enumeration: lists session names from the mux
    /// backend (never capture-pane) and reconciles them against the daemon's
    /// persisted registry, flagging orphans (live but untracked) and registered
    /// sessions that are not running. Attach one with
    /// `ccteam session attach <slug> [sid]`.
    Ls,
    /// Attach to a session. Reaches a gateway chat-mode bot session
    /// (`ccteam-chat-<slug>-<sid>`) first; with `<slug>` alone, attaches when
    /// the slug has exactly one live chat session, else lists them to
    /// disambiguate. Falls back to the project session (`ccteam-<slug>`) when
    /// no live chat session matches.
    Attach {
        slug: String,
        /// Chat session id (the trailing segment of `ccteam-chat-<slug>-<sid>`).
        /// Omit to auto-resolve a single live chat session for `<slug>`.
        sid: Option<String>,
    },
    /// Pause auto-dispatch for one project. Sets `user_pause_pending`
    /// so the workflow loop stops handing the project fresh work; never
    /// kills the long-running session (CLAUDE.md §三 red line). Mirrors
    /// the `mcp__ccteam__workflow_pause` tool.
    Pause { slug: String },
    /// Resume a paused / escalated project (re-arm phase_state=idle).
    /// Mirrors the `mcp__ccteam__workflow_resume` tool.
    Resume { slug: String },
    /// V0.8.6 W1 — switch a live gateway chat session's role in place.
    /// The IM `/role <role>` command is the primary path (it acts on the
    /// chat's current session inside the running daemon). A one-shot CLI
    /// process cannot mutate the daemon's in-memory session map, so this
    /// subcommand only prints the guidance to use the IM `/role` form.
    Role {
        /// Project slug.
        slug: String,
        /// Gateway session id (as shown by `ccteam session ls`).
        sid: String,
        /// Target role (matches `.claude/agents/<role>.md`).
        role: String,
    },
    /// Replace a chat-mode bot's persona file
    /// (`<project>/.claude/agents/<bot>.md`). `new_persona_md` is the
    /// FULL replacement file content (YAML frontmatter + body); the
    /// caller is responsible for assembling it. The bot picks up the
    /// new persona on the next turn / `/clear`.
    Persona {
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
    Register {
        /// Project slug (workflow.yaml's `name` field).
        #[arg(long)]
        slug: String,
        /// Role within the workflow (matches the `agents.<role>` key).
        #[arg(long)]
        role: String,
        /// Harness vendor — `claude` or `codex`.
        #[arg(long, default_value = "claude")]
        vendor: String,
        /// IM platform — `telegram`, `slack`, `discord`, `lark`, or `mock`.
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
    Unregister {
        /// Project slug.
        #[arg(long)]
        slug: String,
        /// Role within the workflow.
        #[arg(long)]
        role: String,
    },
    /// List registered chat-mode bots (reads the F146 registry at
    /// `~/.ccteam/state/im/registry/<slug>/<role>.json`). Confirms what
    /// `register` wrote — role → @handle → platform/chat_id, plus
    /// live `running` status from the per-bot heartbeat sidecar.
    /// Distinct from `session ls`, which lists *live* gateway sessions;
    /// `bots` lists the on-disk registry.
    Bots {
        /// Optional slug filter. Omit to list bots across all slugs.
        #[arg(long)]
        slug: Option<String>,
        /// Emit a JSON array instead of the human-readable table.
        #[arg(long, default_value_t = false)]
        json: bool,
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

/// v0.8.6 W3/W4a — `ccteam project` subcommand group. Houses the project
/// lifecycle verbs: `ls` / `show` inspect, `new` scaffolds, `stop` halts
/// live sessions, `rm` un-registers (with `--purge` deletes the on-disk
/// footprint). W4a folded the former flat `ls` / `show` / `new` commands
/// in here.
#[derive(Subcommand)]
enum ProjectCommand {
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
    /// V0.4.2 F75: scaffold a fresh project under
    /// `<projects_root>/<team>-<slug>/` (thin wrapper over `ccteam init
    /// --in <projects_root>/<team>-<slug>`). Prepends the team prefix and
    /// installs there. To install in the cwd instead, use `ccteam init`.
    New {
        /// Project slug. Becomes the dir name under `projects_root`.
        slug: String,
        /// Team for the new install. Default `dev`.
        #[arg(long, default_value = "dev")]
        team: String,
    },
    /// Un-register a project: drop its `~/.ccteam/config.yaml::projects[]`
    /// entry + scrub the per-slug `~/.ccteam/` state. With `--purge`,
    /// also delete ccteam's project footprint — `.ccteam/`, the seeded
    /// `.claude/agents/cto.md`, and ccteam's hooks inside
    /// `.claude/settings.local.json`. Never touches `.env`, user
    /// work-roles, `CLAUDE.md` / `AGENTS.md`, or the user's
    /// `settings.json`.
    Rm {
        /// Project slug as listed in `ccteam ls` / registered under
        /// `~/.ccteam/config.yaml::projects[]`.
        slug: String,
        /// Also delete ccteam's project footprint (`.ccteam/`, seeded
        /// `cto.md`, ccteam hook section in `settings.local.json`).
        /// Default leaves the project directory's files in place
        /// (config-only deregister).
        #[arg(long, default_value_t = false)]
        purge: bool,
        /// Print every step that would change the filesystem / config /
        /// daemon roster, but don't touch anything. Combine with
        /// `--purge` to see the full clobber list.
        #[arg(long, default_value_t = false)]
        dry_run: bool,
        /// Skip the CLAUDE.md §三 "永不主动 kill 长 session" refusal
        /// gate (tmux / claude bg / open spawn checks).
        #[arg(long, default_value_t = false)]
        force: bool,
    },
    /// Stop a project's live sessions (tmux chat panes + bg jobs)
    /// WITHOUT removing it — an explicit, resumable user-requested stop.
    /// The project stays registered; re-engaging it resumes by id.
    Stop {
        /// Project slug to stop.
        slug: String,
    },
}

/// v0.8.9 Phase 2 — `ccteam role` subcommand group. Browse the curated
/// ccteam-hub marketplace and one-shot install a plugin into a project's
/// `.claude/`. Distinct from `session role` (the live in-daemon role switch):
/// these are one-shot project-file marketplace/install operations.
#[derive(Subcommand)]
enum RoleCommand {
    /// Search the curated ccteam-hub marketplace. Matches the query
    /// (case-insensitive) against each plugin's id / name / description /
    /// tags. An empty query lists everything. Each row prints the plugin
    /// `id` to pass to `ccteam role add <id>`.
    Search {
        /// Substring query. Omit (or pass "") to list everything.
        #[arg(default_value = "")]
        query: String,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
    /// Install a ccteam-hub plugin into a project's `.claude/`. Fetches the
    /// body from the hub, verifies its sha256 against the index, and writes it
    /// verbatim (the file is already Claude-native — no conversion). On
    /// success, prints a hint to `/role <role>` to switch to it in a chat.
    Add {
        /// Plugin id (as shown by `ccteam role search`).
        id: String,
        /// Rename the installed plugin (file stem). Default: the plugin
        /// `id`. Sanitized to `[a-z0-9_-]`.
        #[arg(long = "as", value_name = "ROLE")]
        as_role: Option<String>,
        /// Target project slug (resolved via `~/.ccteam/config.yaml`).
        /// Default: the current working directory.
        #[arg(long, value_name = "SLUG")]
        project: Option<String>,
        /// Overwrite an existing role file of the same name.
        #[arg(long, default_value_t = false)]
        force: bool,
    },
    /// List the roles already installed in a project's `.claude/agents/`
    /// (wraps the resource-API role reader). Default project = the current
    /// working directory.
    List {
        /// Target project slug. Default: the current working directory.
        #[arg(long, value_name = "SLUG")]
        project: Option<String>,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
}

/// Subcommands hidden under `ccteam internal` — hook handlers, the MCP
/// server, low-level session utilities (peek / progress / send / spawn /
/// resume / attach), the mux hook-emit client, the project probe, and the
/// standalone web server. Not user-facing day to day; the
/// `ccteam-control` skill drives these.
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
    /// `~/.claude.json` `mcpServers.ccteam` by `ccteam config` (the
    /// "register the ccteam MCP server" menu item / `config mcp`) so
    /// daily-driver claude sessions see the ccteam tool surface.
    McpServe,
    /// Attach to a session. Resolves a gateway chat-mode bot session
    /// (`ccteam-chat-<slug>-<sid>`) first: `<slug> <sid>` (or a full session
    /// name) is deterministic; with `<slug>` alone, attaches when the slug has
    /// exactly one live chat session, else lists them to disambiguate. Falls
    /// back to the project session (`ccteam-<slug>`) when no live chat session
    /// matches.
    Attach {
        slug: String,
        /// Chat session id (the trailing segment of `ccteam-chat-<slug>-<sid>`).
        /// Omit to auto-resolve a single live chat session for `<slug>`.
        sid: Option<String>,
    },
    /// Capture the project's pane content without attaching. Resolves a
    /// live chat session (`ccteam-chat-<slug>-<sid>`) first, falling back
    /// to the project pane (`ccteam-<slug>`) — same resolution as `attach`.
    Peek {
        slug: String,
        /// Chat session id; omit to auto-resolve a single live chat session.
        sid: Option<String>,
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
    ///
    /// F87: `disable_help_flag` so a literal `--help` in the message body
    /// is not intercepted by clap as the subcommand's own help. Users who
    /// want help should run `ccteam help internal send` instead.
    #[command(disable_help_flag = true)]
    Send {
        slug: String,
        #[arg(short = 'r', long)]
        role: Option<String>,
        #[arg(long, default_value_t = false)]
        no_spawn: bool,
        /// Message body. Use `-` to read from stdin. Leading hyphens are
        /// accepted as literal text (F87) so `ccteam internal send <slug>
        /// "--help"` forwards the string to the agent instead of
        /// triggering ccteam's own help.
        #[arg(allow_hyphen_values = true)]
        body: String,
    },
    /// Trigger a fresh spawn of `<role>` in `<slug>` with an optional
    /// kick prompt. Writes a `.ccteam/spawn_requests/<role>-<ts>.json`
    /// marker the orchestrator picks up on its next tick.
    ///
    /// F87: `disable_help_flag` so a literal `--help` in the prompt is not
    /// intercepted by clap as the subcommand's own help.
    #[command(disable_help_flag = true)]
    Spawn {
        slug: String,
        role: String,
        /// Optional initial prompt. Use `-` to read from stdin. Leading
        /// hyphens are accepted as literal text (F87).
        #[arg(allow_hyphen_values = true)]
        prompt: Option<String>,
    },
    /// V0.8 rmux — mux backend utilities. Today the only subcommand is
    /// `hook-emit`, the W6 daemon-bus hook reroute client (active only
    /// when `CCTEAM_HOOK_VIA_DAEMON=1`; see
    /// `ccteam internal mux hook-emit --help`).
    Mux {
        #[command(subcommand)]
        cmd: MuxCommand,
    },
    /// V0.6.6 F167 — probe a repo root and emit the detected
    /// project kind (monorepo / single-repo / docs-only / scripts-only
    /// / empty), the top-3 languages, a tests-present flag, and the
    /// `probable_scope` paths the `/ccteam-creator` skill should
    /// pre-populate into the rendered `workflow.yaml::agents.<role>.
    /// scope` field. Pure read-only file-existence sweep — no source
    /// parsing, no LLM calls.
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
    /// Serve the ccteam web UI standalone (the gateway daemon embeds
    /// this by default; this subcommand runs it on its own bind for
    /// headless / custom deployments).
    Web {
        /// Listen address. Default `0.0.0.0:7331` so host deployments
        /// reach the LAN out of the box; auth is auto-enabled on
        /// non-loopback. Use `127.0.0.1:7331` for loopback-only
        /// (auth then disabled). Requires token auth on non-loopback
        /// unless `--no-auth`.
        #[arg(long, default_value = "0.0.0.0:7331")]
        bind: String,
        /// Disable token auth on write endpoints. DANGEROUS on
        /// non-loopback bind — prints a 5-second warning before
        /// listening.
        #[arg(long, default_value_t = false)]
        no_auth: bool,
        /// Custom path to read the auth token from (default
        /// `~/.ccteam/web-token`).
        #[arg(long, value_name = "PATH")]
        token_file: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum HookCommand {
    /// Append one event line to ~/.ccteam/progress/<slug>.jsonl.
    /// `event_type` is the `event` field on the resulting JSONL record
    /// (e.g. "PreToolUse" / "Stop" / "session_start").
    ProgressAppend { event_type: String },
    /// SessionStart hook: a validating no-op seam. Validates the hook
    /// stdin `cwd` shape (fails loudly if missing) but performs no
    /// filesystem side effects — no `.ccteam/ready` marker is written.
    /// Kept registered for future per-session bootstrap work.
    LoadContext,
    /// V0.2 M0.19.3 PreToolUse hook for `AskUserQuestion`. Returns a
    /// `permissionDecision: deny` so the assistant routes through the
    /// outbox / clarify protocol instead of synchronously waiting on
    /// an offline user.
    InterceptAsk,
    /// v0.8.7 W2 (DB.3) — `PermissionRequest` hook for HITL chat sessions.
    /// Fires only for a non-allowlist tool (Claude's ask-path); turns it
    /// into an IM approve/deny round-trip over the daemon mcp.sock and
    /// returns the `behavior: allow|deny` decision. Fail-safe deny.
    PermissionRequest,
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

    // v0.8.20 — `--home <dir>` overrides CCTEAM_HOME for this invocation (a
    // second, fully isolated instance, e.g. `~/.ccteam2`). Set before the F173
    // pin below so every path resolution + child spawn sees it.
    if let Some(home) = &cli.home {
        std::env::set_var("CCTEAM_HOME", home);
    }

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
            owner,
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
                    owner,
                },
            )?;
            print!("{report}");
            Ok(())
        }
        Command::Start {
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
        } => run_start(
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
        Command::Status => run_status(),
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
        // v0.8.6 W3/W4a — `ccteam project <ls|show|new|stop|rm>` group.
        Command::Project { cmd } => match cmd {
            ProjectCommand::Ls { format } => run_ls(format),
            ProjectCommand::Show { slug, format } => match slug {
                Some(s) => run_show(&s, format),
                None => show_slug_picker(),
            },
            ProjectCommand::New { slug, team } => run_new(slug, team),
            ProjectCommand::Rm {
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
            ProjectCommand::Stop { slug } => {
                let paths = CcteamPaths::from_env()?;
                let report = commands::run_project_stop(&paths, &slug)?;
                print!("{report}");
                Ok(())
            }
        },
        // v0.8.6 W4a — `ccteam session <...>` group.
        Command::Session { cmd } => run_session(cmd),
        // v0.8.7 W3 — `ccteam role <search|add|list>` group.
        Command::Role { cmd } => run_role(cmd),
        Command::Doctor {
            dry_run,
            tool_surface,
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
                tool_surface,
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
        Command::Config { args } => run_config(args),
    }
}

/// v0.8.6 Item 4 — dispatch `ccteam config`. The interactive menu and the
/// headless forms both live in `commands`; this only routes the positional
/// words. Forms:
///
/// - (no args) → interactive menu (`commands::run_config_menu`)
/// - `mcp` → register/refresh the ccteam MCP server (headless escape hatch
///   for the menu's item 1, so installers / CI can wire MCP without a TTY)
/// - `show` → print preferences
/// - `get <key>` → read one preference
/// - `<key>` → read one preference (single non-keyword word)
/// - `<key> <value>` → set one preference (the bare two-arg form)
fn run_config(args: Vec<String>) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    match args.as_slice() {
        [] => {
            let out = commands::run_config_menu(&paths)?;
            print!("{out}");
            Ok(())
        }
        [one] if one == "mcp" => {
            let out = commands::run_config_install_mcp()?;
            print!("{out}");
            Ok(())
        }
        [one] if one == "show" => {
            let out = commands::run_prefs_show(&paths)?;
            print!("{out}");
            Ok(())
        }
        [one] => {
            // Single non-keyword word → read that preference key.
            let out = commands::run_prefs_get(&paths, one)?;
            println!("{out}");
            Ok(())
        }
        [first, second] if first == "get" => {
            let out = commands::run_prefs_get(&paths, second)?;
            println!("{out}");
            Ok(())
        }
        [key, value] => {
            // Bare two-arg form `config <key> <value>` → set the pref.
            let out = commands::run_prefs_set(&paths, key, value)?;
            println!("{out}");
            Ok(())
        }
        // clap caps this at 2 positionals (`num_args = 0..=2`), so the
        // 3+ arm is unreachable; keep it total for the compiler.
        _ => anyhow::bail!("ccteam config: too many arguments (expected at most `<key> <value>`)"),
    }
}

/// v0.8.6 W4a — dispatch `ccteam session <subcmd>`. The bot-admin verbs
/// (`persona` / `add-tool` / `register` / `unregister` / `bots`) call the
/// same `commands::run_admin_*` handlers the retired `ccteam admin` group
/// used; the session verbs (`ls` / `attach` / `pause` / `resume`) call the
/// same handlers the retired flat top-level commands used. `role` is the
/// CLI counterpart of the IM `/role` (see the handler body).
fn run_session(cmd: SessionCommand) -> Result<()> {
    match cmd {
        SessionCommand::Ls => commands::run_sessions(),
        SessionCommand::Attach { slug, sid } => run_internal_attach(&slug, sid.as_deref()),
        SessionCommand::Pause { slug } => run_pause(&slug),
        SessionCommand::Resume { slug } => run_resume(&slug),
        SessionCommand::Role { slug, sid, role } => run_session_role(&slug, &sid, &role),
        SessionCommand::Persona {
            slug,
            bot,
            new_persona_md,
        } => {
            let paths = CcteamPaths::from_env()?;
            let body = read_inline_or_stdin(&new_persona_md)?;
            let out = commands::run_admin_change_persona(&paths, &slug, &bot, &body)?;
            println!("{out}");
            Ok(())
        }
        SessionCommand::AddTool {
            slug,
            bot,
            tool_descriptor,
        } => {
            let paths = CcteamPaths::from_env()?;
            let out = commands::run_admin_add_tool(&paths, &slug, &bot, &tool_descriptor)?;
            println!("{out}");
            Ok(())
        }
        SessionCommand::Register {
            slug,
            role,
            vendor,
            platform,
            chat_id,
            chat_handle,
            project_dir,
        } => {
            let paths = CcteamPaths::from_env()?;
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
        SessionCommand::Unregister { slug, role } => {
            let paths = CcteamPaths::from_env()?;
            let out = commands::run_admin_unregister_bot(&paths, &slug, &role)?;
            println!("{out}");
            Ok(())
        }
        SessionCommand::Bots { slug, json } => {
            let paths = CcteamPaths::from_env()?;
            let out = commands::run_admin_list_bots(&paths, slug.as_deref(), json)?;
            println!("{out}");
            Ok(())
        }
    }
}

/// v0.8.6 W4a — `ccteam session role <slug> <sid> <role>`. The W1 role
/// switch (`/role <role>`) is a live-gateway operation: it tears down the
/// current chat session's pane and re-spawns a fresh `--agent <role>`
/// thread, reusing the same gateway session id, by mutating the running
/// daemon's in-memory `current_session` / `sessions` maps
/// (`Gateway::switch_current_role`). A one-shot CLI process has no handle
/// on that in-memory state, so there is no behavior to delegate to from
/// here. Rather than invent a second, divergent role-mutation path, this
/// CLI form points the operator at the supported IM `/role` command.
fn run_session_role(slug: &str, sid: &str, role: &str) -> Result<()> {
    anyhow::bail!(
        "`ccteam session role` is not a one-shot operation: switching a live \
         chat session's role re-spawns its pane inside the running gateway. \
         Use the IM `/role {role}` command in the chat that owns session \
         `{sid}` (project `{slug}`) — it switches the current session in place.",
    )
}

/// v0.8.7 W3 — `ccteam role <search|add|list>` dispatcher. `search` is
/// offline (no project / no network); `add` / `list` resolve a project dir
/// from `--project <slug>` (or the cwd) and call into the commands layer.
fn run_role(cmd: RoleCommand) -> Result<()> {
    match cmd {
        RoleCommand::Search { query, format } => {
            let paths = CcteamPaths::from_env()?;
            let out = commands::run_role_search(&paths, &query, format)?;
            print!("{out}");
            Ok(())
        }
        RoleCommand::Add {
            id,
            as_role,
            project,
            force,
        } => {
            let paths = CcteamPaths::from_env()?;
            let out =
                commands::run_role_add(&paths, &id, as_role.as_deref(), project.as_deref(), force)?;
            print!("{out}");
            Ok(())
        }
        RoleCommand::List { project, format } => {
            let paths = CcteamPaths::from_env()?;
            let out = commands::run_role_list(&paths, project.as_deref(), format)?;
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

/// Dispatch `ccteam internal <subcmd>` — the hidden low-level surface
/// (hook handlers, MCP server, session utilities, mux hook-emit, project
/// probe, standalone web server).
fn run_internal(cmd: InternalCommand) -> Result<()> {
    match cmd {
        InternalCommand::Hook { cmd } => run_hook(cmd),
        InternalCommand::McpServe => run_mcp_serve(),
        InternalCommand::Attach { slug, sid } => run_internal_attach(&slug, sid.as_deref()),
        InternalCommand::Peek { slug, sid } => run_internal_peek(&slug, sid.as_deref()),
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
        InternalCommand::Mux { cmd } => run_mux(cmd),
        InternalCommand::ProbeProject { path, json } => {
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
        InternalCommand::Web {
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
    // Hookless session (stream-json): no-op, same as `run_hook`. This is the
    // W6 daemon-bus form that bypasses hook.sh's guard.
    if std::env::var_os("CCTEAM_HOOKLESS").is_some() {
        return Ok(());
    }
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

/// `ccteam session attach <slug> [sid]` / `ccteam internal attach <slug>
/// [sid]` — reach gateway chat-mode bot
/// sessions first (the project-oriented [`commands::run_attach`] cannot see
/// `ccteam-chat-*`), then fall back to the project session when no live chat
/// session matches `slug`.
fn run_internal_attach(slug: &str, sid: Option<&str>) -> Result<()> {
    if commands::try_attach_chat_session(slug, sid)? {
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
    // v0.8.22 — a truly bare `ccteam doctor` (no flags at all) now runs
    // the full readiness checkup instead of only the implicit pricing
    // check; any explicit flag (including the now-hidden migration/
    // repair ones) still dispatches through `commands::run_doctor`
    // completely unchanged, so every existing flag-driven test keeps
    // pinning the exact same behavior.
    if doctor::is_bare_invocation(&opts) {
        let (body, any_fail) = doctor::run_readiness_checkup(&paths);
        print!("{body}");
        if any_fail {
            std::process::exit(1);
        }
        return Ok(());
    }
    let body = commands::run_doctor(&paths, opts)?;
    print!("{body}");
    Ok(())
}

fn run_hook(cmd: HookCommand) -> Result<()> {
    // Hookless session (stream-json protocol): the spawn injects
    // CCTEAM_HOOKLESS=1 so the hook chain never fires (no SessionStart-POST
    // init deadlock, no double-emit — events come from the child's stdout).
    // `hook.sh` already short-circuits before reaching here, but the W6
    // `mux hook-emit` form and any direct CLI invocation bypass hook.sh, so
    // guard here too. A no-op (Ok / empty stdout) = "no opinion" for any
    // decision hook → the tool proceeds.
    if std::env::var_os("CCTEAM_HOOKLESS").is_some() {
        return Ok(());
    }
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
        // v0.8.5 D6 — intercept-ask now reads the AskUserQuestion `tool_input`
        // from stdin (the chat variant routes the question to IM); the bg
        // variant ignores the payload and still denies.
        HookCommand::InterceptAsk => ("intercept-ask", None, true),
        // v0.8.7 W2 (DB.3) — reads the PermissionRequest payload from stdin
        // and prints the allow/deny decision JSON.
        HookCommand::PermissionRequest => ("permission-request", None, true),
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

    // Ensure the global `~/.ccteam/` home (canonical dirs +
    // `hooks/hook.sh` dispatcher) is complete BEFORE the daemon starts
    // serving, so any session it spawns — including projects created via
    // the web / IM path while the daemon is up — finds the hook.sh the
    // project `.claude/settings.local.json` references. Idempotent.
    // Best-effort: a failure here must not crash the daemon (the
    // per-create-path `ensure_ccteam_home` is the real safety net).
    if let Err(err) = ccteam_core::ensure_ccteam_home(&paths) {
        tracing::warn!(error = %err, "could not ensure ~/.ccteam/ home at daemon start; sessions may hit a missing hook.sh until `ccteam doctor --install-hooks`");
    }

    // V0.4.0 F60: the shipped team seed writer was deleted with the
    // phase machinery (F63 will reintroduce a workflow seed). Daemon
    // start no longer self-heals — operators supply their own
    // `~/.ccteam/teams/<name>/team.yaml`.
    //
    // v0.8.6 (review-fix #3): the old "phases dir not found; run ccteam
    // init to unpack templates" hint was orchestrator-era dead code —
    // `ccteam init` no longer creates `phases/` (and templates are no
    // longer unpacked), so it printed spuriously on every clean start.
    // Removed.

    if !paths.projects_root.exists()
        || std::fs::read_dir(&paths.projects_root)
            .map(|mut it| it.next().is_none())
            .unwrap_or(true)
    {
        eprintln!(
            "ccteam: no projects under {} yet.\n  start one in another terminal: ccteam project new <slug> (e.g. `ccteam project new demo`)",
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

    // Re-publish the Telegram command menu (`setMyCommands`) on EVERY
    // `ccteam start`, BEFORE the single-instance pidfile guard below.
    // The daemon also registers the menu at its own startup
    // (`run_daemon_with_shutdown`), but that only fires when a *fresh*
    // daemon boots: if one is already running, the `write_pidfile` guard
    // aborts this `ccteam start` before any registration happens, and a
    // token configured *after* first start never re-registers. Doing it
    // here makes the menu refresh reliably on every start regardless —
    // idempotent (`setMyCommands` replaces the whole menu) and inert to a
    // live daemon/sessions (it is one HTTPS POST to the Bot API). Skipped
    // when IM is off (`--no-imd`); warn-only so a Bot-API hiccup never
    // blocks startup. A short throwaway runtime keeps this self-contained
    // ahead of the main runtime built below.
    if !imd.disabled {
        match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => {
                if let Err(err) = rt.block_on(ccteam_im::refresh_telegram_command_menu(None)) {
                    tracing::warn!(
                        error = %err,
                        "ccteam start: Telegram command-menu refresh (setMyCommands) failed; menu may be stale"
                    );
                }
            }
            Err(err) => {
                tracing::warn!(error = %err, "ccteam start: could not build runtime for Telegram menu refresh; skipping");
            }
        }
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

        // V0.8.6 W5b — composition root for the resource-API spine. When BOTH
        // web and the IM gateway run in this process, build the shared
        // `Arc<Mutex<Gateway>>` ONCE here, before either task spawns, and hand
        // the SAME handle to the web `AppState` (read/drive sessions over HTTP)
        // and the daemon (owns the session map). Building it pre-spawn removes
        // the web-vs-IM spawn-order race. When web is off (standalone "internal
        // web" has no daemon gateway) or IM is off, leave it `None`: the daemon
        // then builds + owns its own gateway exactly as before, and any web
        // session endpoint returns 503 (gateway is `None` in `AppState`).
        // v0.8.22 P0-2 — `build_gateway_for_daemon` now also returns the
        // stream-json Claude adapter singleton it baked into the gateway, so
        // it can be threaded into `DaemonArgs::claude_stream_json_adapter`
        // below: the daemon wires the production HITL resolver onto this
        // EXACT adapter once pending + the event sink exist.
        let (shared_gateway, shared_claude_stream_json): (
            Option<std::sync::Arc<tokio::sync::Mutex<ccteam_im::gateway::Gateway>>>,
            Option<std::sync::Arc<ccteam_harness::ClaudeStreamJsonAdapter>>,
        ) = if web.disabled || imd.disabled {
            (None, None)
        } else {
            match ccteam_im::build_gateway_for_daemon(None) {
                Ok((g, adapter)) => (
                    Some(std::sync::Arc::new(tokio::sync::Mutex::new(g))),
                    Some(adapter),
                ),
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        "ccteam start: failed to build shared gateway; web session API will be unavailable (503), daemon builds its own"
                    );
                    (None, None)
                }
            }
        };

        let signal_task = tokio::spawn(async move {
            wait_for_shutdown_signal().await;
            let _ = shutdown_tx.send(true);
        });

        // V0.8.4 P2b — shared gateway-event channel: the IM daemon
        // consumes it; the mcp.sock handler + `POST /mcp` clone the sender so
        // `chat_send_file` reuses the same outbound funnel. Only created
        // when IM is enabled (no consumer ⇒ nothing to deliver to).
        // Built BEFORE the web task so the web state factory can hand the
        // same pieces into `AppState::with_mcp` (v0.9 T4).
        let (gw_event_tx, gw_event_rx) = if imd.disabled {
            (None, None)
        } else {
            let (t, r) =
                tokio::sync::mpsc::unbounded_channel::<ccteam_im::gateway::GatewayEvent>();
            (Some(t), Some(r))
        };

        // v0.8.5 D6 — one shared pending-interaction registry handed to BOTH
        // the gateway (resolves inbound clicks) and the mcp.sock / `POST /mcp`
        // handlers (registers External-origin `interaction/ask` prompts from the
        // AskUserQuestion hook). The shared `Arc` is the bridge across the two
        // scopes. Only when IM is enabled (no gateway ⇒ no one to resolve).
        let pending_registry: Option<
            std::sync::Arc<tokio::sync::Mutex<ccteam_im::pending::PendingInteractions>>,
        > = if imd.disabled {
            None
        } else {
            Some(std::sync::Arc::new(tokio::sync::Mutex::new(
                ccteam_im::pending::PendingInteractions::new(),
            )))
        };

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
            // V0.8.6 W5b — clone the shared gateway handle into the web state
            // factory so HTTP session endpoints drive the same session map the
            // daemon owns.
            let web_gateway = shared_gateway.clone();
            // v0.9 T4 — same sink/pending the mcp.sock handler gets, so
            // `POST /mcp` can drive session_* / chat_send_file / HITL.
            let web_mcp_sink = gw_event_tx.clone();
            let web_mcp_pending = pending_registry.clone();
            Some(tokio::spawn(async move {
                ccteam_web::serve_with_state_factory_and_shutdown(
                    opts,
                    move |paths, auth| {
                        let mut state = ccteam_web::AppState::with_auth(paths, auth);
                        if let Some((inbound, outbound, backlog, conns)) = web_bridge {
                            state = state.with_chat_bridge(inbound, outbound, backlog, conns);
                        }
                        if let Some(gw) = web_gateway {
                            state = state.with_gateway(gw);
                        }
                        state = state.with_mcp(web_mcp_sink, web_mcp_pending);
                        state
                    },
                    async move {
                        let _ = rx.changed().await;
                    },
                )
                .await
            }))
        };

        let imd_handle = if imd.disabled {
            tracing::info!("ccteam start: --no-imd set; IM gateway task skipped");
            None
        } else {
            let mut rx = shutdown_rx.clone();
            let mut args = ccteam_im::DaemonArgs {
                gateway_event_tx: gw_event_tx.clone(),
                gateway_event_rx: gw_event_rx,
                pending: pending_registry.clone(),
                // V0.8.6 W5b — reuse the gateway the composition root built so
                // the daemon and the web `AppState` drive ONE session map. When
                // `None` (web off), the daemon builds + owns its own.
                gateway: shared_gateway.clone(),
                // v0.8.22 P0-2 — pairs with `gateway`: the stream-json Claude
                // adapter singleton `build_gateway_for_daemon` baked into it,
                // so the daemon can wire the production HITL resolver onto
                // the EXACT adapter this gateway spawns sessions through.
                claude_stream_json_adapter: shared_claude_stream_json.clone(),
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
        let mcp_pending = pending_registry.clone();
        // v0.8.7 W1 — hand the SAME shared gateway Arc to the mcp.sock handler
        // so the cto's `session_*` tools drive the daemon's session map
        // directly (cloning an Arc is cheap + acyclic; the daemon and web
        // already hold the same handle). `None` when web/IM is off → the
        // session tools then report "gateway not running".
        let mcp_gateway = shared_gateway.clone();
        let mcp_handle = tokio::spawn(async move {
            serve_mcp_socket(paths, mcp_sink, mcp_pending, mcp_gateway, async move {
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
    sink: Option<ccteam_im::mcp::GatewayEventSink>,
    pending: Option<ccteam_im::mcp::PendingRegistry>,
    gateway: Option<ccteam_im::mcp::GatewayHandle>,
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
                let pending = pending.clone();
                let gateway = gateway.clone();
                tokio::spawn(async move {
                    if let Err(err) =
                        handle_mcp_socket_connection(paths, sink, pending, gateway, stream).await
                    {
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
    _sink: Option<ccteam_im::mcp::GatewayEventSink>,
    _pending: Option<ccteam_im::mcp::PendingRegistry>,
    _gateway: Option<ccteam_im::mcp::GatewayHandle>,
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
    sink: Option<ccteam_im::mcp::GatewayEventSink>,
    pending: Option<ccteam_im::mcp::PendingRegistry>,
    gateway: Option<ccteam_im::mcp::GatewayHandle>,
    stream: tokio::net::UnixStream,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = tokio::io::BufReader::new(reader).lines();
    let dispatch = ccteam_im::mcp::McpDispatch {
        paths,
        sink,
        pending,
        gateway,
    };
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
        if let Some(response) = dispatch.dispatch(req).await {
            let mut out = serde_json::to_string(&response)?;
            out.push('\n');
            writer.write_all(out.as_bytes()).await?;
            writer.flush().await?;
        }
    }
    Ok(())
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

fn run_status() -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    use ccteam_core::check_daemon_health;

    println!("ccteam status");
    println!();

    // ① Daemon health.
    let health = check_daemon_health(&paths);
    let daemon_up = health.is_healthy();
    println!("  daemon:  {}", health.describe());
    println!();

    // ② Projects, each with its tracked sessions nested underneath.
    //
    // v0.8.8 F3 — the old flat "projects" / "running sessions" / "recent
    // events" trio is collapsed into one tree: a project row (slug · age ·
    // last-event · STUCK/OK verdict) followed by its sessions, grouped from
    // the daemon's persisted records via `tracked_chat_sessions` (same
    // out-of-process source `session ls` uses, so the two never drift).
    let projects = ccteam_core::queries::collect_projects(&paths).unwrap_or_default();

    // Group tracked sessions by project slug. A missing / unreadable registry
    // is non-fatal — just an empty map (no sessions shown).
    let tracked = ccteam_im::gateway::tracked_chat_sessions(&paths.root).unwrap_or_default();
    let mut sessions_by_project: std::collections::BTreeMap<
        String,
        Vec<ccteam_im::gateway::TrackedSessionRow>,
    > = std::collections::BTreeMap::new();
    for row in tracked {
        sessions_by_project
            .entry(row.project.clone())
            .or_default()
            .push(row);
    }

    if projects.is_empty() {
        println!("  projects: (none — `ccteam project new <slug>` to create one, e.g. `ccteam project new demo`)");
    } else {
        println!("  projects ({}):", projects.len());
        // Classify each project from the same file-backed progress truth that
        // the JSON status and web Status rail read.
        let mut needs_attention: Vec<(String, Option<String>, String)> = Vec::new();
        for p in &projects {
            let age = humanize_secs(p.age_seconds);
            let events =
                ccteam_core::progress::read_all_events(&paths.progress_jsonl(&p.state.slug))
                    .unwrap_or_default();
            let now = chrono::Utc::now();
            let project_fallback = events
                .last()
                .filter(|event| ccteam_core::progress::event_sid(event).is_none());
            let fallback_activity = ccteam_core::stall::classify_progress_activity(
                project_fallback,
                p.stall_silent_seconds,
                now,
            );
            let mut project_status = fallback_activity.status;
            let mut attention_sid: Option<String> = None;
            let mut attention_silent_seconds = fallback_activity
                .event_age_seconds
                .unwrap_or(p.stall_silent_seconds);

            let mut session_lines: Vec<(String, String, String, String, String)> = Vec::new();
            if let Some(rows) = sessions_by_project.get(&p.state.slug) {
                for s in rows {
                    let activity = ccteam_core::stall::classify_progress_activity_for_sid(
                        &events,
                        &s.sid,
                        p.stall_silent_seconds,
                        now,
                    );
                    if progress_status_rank(activity.status) > progress_status_rank(project_status)
                    {
                        project_status = activity.status;
                        attention_sid = Some(s.sid.clone());
                        attention_silent_seconds =
                            activity.event_age_seconds.unwrap_or(p.stall_silent_seconds);
                    }
                    let role = if s.role.is_empty() { "-" } else { &s.role };
                    let session_status = if daemon_up {
                        activity.status.activity.to_string()
                    } else {
                        "registered (daemon down)".to_string()
                    };
                    let last_event = activity
                        .event_age_seconds
                        .map(humanize_secs)
                        .unwrap_or_else(|| "-".to_string());
                    session_lines.push((
                        role.to_string(),
                        s.vendor.clone(),
                        session_status,
                        s.sid.clone(),
                        last_event,
                    ));
                }
            }

            let verdict = project_status.verdict;
            let silent = humanize_secs(attention_silent_seconds);
            if verdict != "OK" {
                needs_attention.push((p.state.slug.clone(), attention_sid, silent.clone()));
            }
            println!(
                "    {:<32}  age {:>8}  last-event {:>8}  {}",
                p.state.slug, age, silent, verdict
            );

            for (role, vendor, session_status, sid, last_event) in session_lines {
                println!(
                    "        {:<10}  {:<7}  {:<26}  {:<6}  last-event {}",
                    role, vendor, session_status, sid, last_event
                );
            }
        }
        // One actionable hint line per warn-or-higher project so the
        // operator knows the exact peek → attach takeover sequence.
        for (slug, sid, silent) in &needs_attention {
            let hint = match sid {
                Some(sid) => commands::stall_takeover_hint_for_session(slug, sid, silent),
                None => commands::stall_takeover_hint(slug, silent),
            };
            println!("    {hint}");
        }
    }
    println!();

    // ④ Web token + URL (two lines).
    //
    // - `web token:` prints the BARE hex (no `ccteam:` prefix) so it can be
    //   copy-pasted into tools that add their own prefixing.
    // - `web url:` embeds the token WITH the `ccteam:` prefix (the form the web
    //   console expects in the query string) at the LAN IP + port 7331.
    let token_path = ccteam_web::token::default_token_path(&paths);
    if token_path.exists() {
        match ccteam_web::token::load_existing(&token_path) {
            Ok(hex) => {
                println!("  web token: {hex}");
                match first_lan_ipv4() {
                    Some(ip) => {
                        println!("  web url:   http://{ip}:7331/?token=ccteam:{hex}")
                    }
                    None => println!("  web url:   <no LAN ip detected> (use http://localhost:7331/?token=ccteam:{hex})"),
                }
            }
            Err(_) => println!("  web token: <unreadable> ({})", token_path.display()),
        }
    } else {
        println!(
            "  web token: <none yet — generated on first non-loopback `ccteam start`> ({})",
            token_path.display()
        );
    }

    // ⑤ v0.8.20 F3 — per-user (tenant) login links. `ccteam status` runs on the
    //    box = the admin/operator context, so it surfaces every tenant's personal
    //    `?token=ccteam:<hex>` link (the value the admin hands out / re-sends).
    //    Tenants never see each other's; this local admin view is the CLI peer of
    //    the web user-management "copy link" action (GET /api/v1/users/{id}/link).
    let tenants = ccteam_core::tenants::TenantRegistry::load(&paths.users_dir());
    if !tenants.list().is_empty() {
        println!();
        println!("  web tenants ({}):", tenants.list().len());
        let host = first_lan_ipv4()
            .map(|ip| format!("http://{ip}:7331"))
            .unwrap_or_else(|| "http://localhost:7331".to_string());
        for t in tenants.list() {
            println!("    {:<14}  {host}/?token=ccteam:{}", t.handle, t.web_token);
        }
    }
    Ok(())
}

fn progress_status_rank(status: ccteam_core::stall::ProgressStallStatus) -> u8 {
    match status.level {
        "stuck" => 3,
        "warn" => 2,
        _ => 1,
    }
}

/// v0.8.8 F3 — predicate for a LAN-reachable IPv4: a private
/// (RFC1918) address that is neither loopback (`127.0.0.0/8`) nor
/// link-local (`169.254.0.0/16`). Pure so the interface-walk in
/// [`first_lan_ipv4`] can be unit-tested without real interfaces.
fn is_lan_ipv4(ip: &std::net::Ipv4Addr) -> bool {
    !ip.is_loopback() && !ip.is_link_local() && ip.is_private()
}

/// v0.8.8 F3 — first non-loopback, non-link-local, private IPv4 of any local
/// interface, for the `ccteam status` web URL line (so the operator gets a
/// LAN-reachable address, not `127.0.0.1`). Returns `None` when no private
/// IPv4 is configured (the URL line then degrades to a localhost hint).
///
/// Uses `getifaddrs(3)` directly — `ccteam-cli` already depends on `libc`
/// (zero new deps). The unsafe FFI is fully contained here: it walks the
/// linked list, reads only `AF_INET` entries, converts the network-order
/// `s_addr` to host order for `Ipv4Addr`, and always `freeifaddrs`-frees the
/// list before returning.
#[cfg(unix)]
fn first_lan_ipv4() -> Option<std::net::Ipv4Addr> {
    use std::net::Ipv4Addr;

    // SAFETY: `getifaddrs` writes a heap-allocated linked list into `ifap` on
    // success (returns 0) which we must `freeifaddrs`. We only read fields
    // through valid non-null pointers and free exactly once before returning.
    unsafe {
        let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut ifap) != 0 {
            return None;
        }
        let mut found: Option<Ipv4Addr> = None;
        let mut cur = ifap;
        while !cur.is_null() {
            let ifa = &*cur;
            let addr = ifa.ifa_addr;
            if !addr.is_null() && (*addr).sa_family as i32 == libc::AF_INET {
                // The generic `sockaddr` for an AF_INET entry is really a
                // `sockaddr_in`; read its network-order `s_addr`.
                let sin = &*(addr as *const libc::sockaddr_in);
                let be = sin.sin_addr.s_addr; // network byte order
                let ip = Ipv4Addr::from(u32::from_be(be));
                if is_lan_ipv4(&ip) {
                    found = Some(ip);
                    break;
                }
            }
            cur = ifa.ifa_next;
        }
        libc::freeifaddrs(ifap);
        found
    }
}

#[cfg(not(unix))]
fn first_lan_ipv4() -> Option<std::net::Ipv4Addr> {
    None
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
        println!("no projects yet — `ccteam project new <slug>` to make one, e.g. `ccteam project new demo`.");
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
        anyhow::bail!("ccteam project new: --team must be non-empty");
    }
    // V0.4.3 F76: fail loud at the CLI boundary on invalid slug grammar
    // (whitespace / unicode / leading dash etc.) so we don't spawn
    // `~/projects/<garbage>/` and leave junk for the user to clean up.
    let validated = ccteam_core::validate_slug_format(&slug)
        .with_context(|| format!("ccteam project new {slug:?}"))?;
    let paths = CcteamPaths::from_env()?;
    // V0.4.2 F75: `ccteam project new <slug>` delegates to `ccteam init` with
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

fn run_internal_peek(slug: &str, sid: Option<&str>) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    let body = commands::run_peek_with_role(&paths, slug, sid)?;
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
mod lan_ip_tests {
    use super::*;
    use std::net::Ipv4Addr;

    /// v0.8.8 F3 — the LAN predicate accepts only private addresses and
    /// rejects loopback / link-local. Exercises the exact tiers the recon
    /// test plan named (192.168 / 127.0.0.1 / 169.254).
    #[test]
    fn is_lan_ipv4_filters_loopback_and_linklocal() {
        // Private (RFC1918) → accepted.
        assert!(is_lan_ipv4(&Ipv4Addr::new(192, 168, 1, 5)));
        assert!(is_lan_ipv4(&Ipv4Addr::new(10, 0, 0, 7)));
        assert!(is_lan_ipv4(&Ipv4Addr::new(172, 16, 3, 9)));
        // Loopback / link-local / public → rejected.
        assert!(!is_lan_ipv4(&Ipv4Addr::new(127, 0, 0, 1)));
        assert!(!is_lan_ipv4(&Ipv4Addr::new(169, 254, 10, 10)));
        assert!(!is_lan_ipv4(&Ipv4Addr::new(8, 8, 8, 8)));
    }

    /// v0.8.8 F3 — querying real interfaces must never panic, and any
    /// address it returns must satisfy the LAN predicate (no loopback /
    /// link-local / public leaking into the web URL).
    #[test]
    fn first_lan_ipv4_does_not_panic_and_is_lan() {
        if let Some(ip) = first_lan_ipv4() {
            assert!(is_lan_ipv4(&ip), "returned non-LAN ip: {ip}");
        }
    }
}
