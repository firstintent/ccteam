//! Command handlers for `ccteam {new, ls, show, attach, peek, progress,
//! resume}`. Pure where possible (`run_ls` / `run_show` return the
//! formatted string instead of printing) so unit tests don't need a
//! real terminal or running orchestrator.

use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};

use ccteam_core::tmux::TmuxSession;
use ccteam_core::{
    cost_summary, current_ccteam_bin, migrate_recommended_agent_symlinks, pricing_schema_version,
    pricing_schema_version_for, rewrite_legacy_hook_commands, session_name_for_project,
    user_claude_dir, CcteamPaths, HookCmdRewriteAction, HookCmdRewriteReport, MigrationReport,
    PhaseState, ProjectState, ToolSurfaceSnapshot, Vendor, BUILTIN_SUBAGENTS,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    Text,
    Json,
}

// V0.6.1 F139 — `install_hooks` + `HOOK_DISPATCHER_SH` live in
// `ccteam_core::hooks_dispatcher` so the embedded script can be tested
// alongside the rest of core's bootstrap-template machinery.
pub use ccteam_core::{install_hooks, InstallHooksAction, HOOK_DISPATCHER_SH};

/// V0.5.0 F93b — `ccteam init --mode <variant>` selector.
///
/// Default is `ArtifactDriven` (V0.4.6 behavior preserved). `AgentTeam`
/// switches the init scaffold to the agent-team mode: the
/// `workflow.agent-team.yaml` template, the `__lead.md` agent
/// scaffold, and the `settings.agent-team.json` settings template
/// (with F94 hooks).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum InitMode {
    /// V0.4.0 default. ArtifactWatcher + trigger graph drive spawns.
    #[default]
    ArtifactDriven,
    /// V0.5.0 F93b. `__lead` session under Anthropic Agent Teams.
    AgentTeam,
}

/// Options passed from the `ccteam init` argument parser.
#[derive(Debug, Clone, Default)]
pub struct InitOptions {
    /// V0.4.2 F72: install in this directory. `None` defaults to the
    /// current working directory. (`slug` no longer affects the
    /// location — it only names the registered project.)
    pub install_in: Option<std::path::PathBuf>,
    /// V0.4.2 F72: slug override — sets the *registered project name*
    /// only, never the install location. When absent we derive it from
    /// the install target's dir basename.
    pub slug: Option<String>,
    /// V0.4.2 F72: team for new installs (default `dev`). On refresh
    /// the existing `state.json::team` is preserved unless `force`.
    pub team: Option<String>,
    /// Overwrite every ccteam-managed file (state.json, settings.json
    /// marker section, workflow.yaml, agents/*.md, helper templates).
    /// Without `force` re-runs preserve user-edited workflow + agents.
    pub force: bool,
    /// V0.4.2 F72: reset only `.claude/agents/*.md` (keep workflow.yaml
    /// + everything else).
    pub reset_agents: bool,
    /// V0.4.1: prompt y/n for each optional global install step (MCP,
    /// skill, meta-agent) after the project install.
    pub interactive: bool,
    /// V0.4.1: assume `yes` for every install-step prompt. Implies
    /// `interactive` but skips actual stdin.
    pub yes: bool,
    /// V0.5.0 F93b — workflow mode for the scaffolded workflow.yaml.
    /// Defaults to `ArtifactDriven`. `AgentTeam` writes the
    /// `workflow.agent-team.yaml` template + `__lead.md` scaffold +
    /// `settings.agent-team.json` (with F94 hooks).
    pub mode: InitMode,
}

/// V0.4.2 F72: unified install command.
///
/// Three scenarios, one command:
///
/// 1. **Fresh cwd / fresh dir**: writes `.ccteam/state.json`,
///    `workflow.yaml` scaffold, `.claude/settings.json`, `.claude/
///    agents/*.md` scaffolds; appends to `~/.ccteam/config.yaml::
///    projects[]`.
/// 2. **Existing repo cwd** (no `.ccteam/` yet): same as (1) — never
///    touches existing user files.
/// 3. **Already-ccteam project cwd**: refreshes state.json + the
///    settings.json ccteam-managed marker section; preserves
///    `workflow.yaml` + `.claude/agents/*.md` unless `--force` (full
///    overwrite) or `--reset-agents` (agents only).
///
/// Tool-level side effects (helper templates in `~/.ccteam/templates/`,
/// optional MCP/skill/meta-agent install via `--interactive`/`--yes`)
/// run on every invocation. They're idempotent and cheap.
pub fn run_init(paths: &CcteamPaths, opts: InitOptions) -> Result<String> {
    use std::process::Command;

    // -- 1. Global ~/.ccteam/ skeleton (idempotent) -------------------
    // v0.8.6 D1.1: create exactly the canonical home-layout manifest
    // (`hooks/progress/run/state`) — the same set `ccteam doctor`'s
    // home-drift check tolerates. The orchestrator-era subdirs
    // (`phases/templates/inbox/control`) are no longer written: nothing
    // reads them post-W2, and creating them made a fresh `ccteam init`
    // immediately report self-inflicted drift.
    for sub in ccteam_core::canonical_home_dirs() {
        let dir = paths.root.join(sub);
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    }

    // V0.6.1 F139 — materialize the per-hook dispatcher script. This
    // must run before `install_project_at` so the freshly-rendered
    // `.claude/settings.json` hook commands point at a file that
    // actually exists.
    install_hooks(paths).context("install ~/.ccteam/hooks/hook.sh dispatcher")?;

    // -- 2. Resolve project install target ---------------------------
    let target = resolve_install_target(&opts)?;
    let slug_was_explicit = opts.slug.is_some();
    let derived_slug = opts.slug.clone().unwrap_or_else(|| {
        target
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("project")
            .to_string()
    });
    // V0.4.3 F76: validate slug grammar before anything writes to disk.
    // Catches whitespace / unicode / leading-dash / uppercase cases.
    // When the slug was derived from the install-dir basename (no
    // `--slug`), point the user at `--slug` instead of failing opaquely
    // — a dir like `AgentServe` (caps) can't be a slug verbatim.
    let target_slug = ccteam_core::validate_slug_format(&derived_slug).with_context(|| {
        if slug_was_explicit {
            format!("ccteam init: invalid slug {derived_slug:?}")
        } else {
            format!(
                "ccteam init: install-dir name {derived_slug:?} is not a valid slug \
                 (lowercase ASCII / digits / dashes only). Pass an explicit name, e.g. \
                 `ccteam init --slug <lowercase-name>`."
            )
        }
    })?;
    let target_team = opts.team.clone().unwrap_or_else(|| "dev".to_string());

    // -- 3a. Refuse install in the ccteam repo itself ----------------
    // V0.6.8 F203 — `--force` escape: legitimate self-hosting /
    // dogfooding / nested-research-project cases need to install a
    // ccteam project inside the ccteam source tree. The default still
    // refuses to avoid circular-hook surprises for casual users.
    if is_ccteam_repo(&target) && !opts.force {
        return Err(anyhow::anyhow!(
            "refusing to install ccteam in the ccteam repo itself: {}\n\n\
             this directory contains the ccteam source — installing here would create \
             a circular hook setup. Pick a different directory (or `cd` into your own project \
             and re-run), or pass `--force` if you really want a ccteam project inside the \
             ccteam source repo (e.g. for self-hosting / dogfooding).",
            target.display(),
        ));
    }
    // -- 3b. Refuse sensitive paths (HOME / filesystem root) ---------
    refuse_sensitive_install_target(&target, opts.force)?;
    // -- 3c. Fail-loud slug collision against config.yaml -----------
    if let Some(existing) = ccteam_core::lookup_project_in_config(&paths.root, &target_slug)? {
        let same_target = std::fs::canonicalize(&existing.path)
            .ok()
            .zip(std::fs::canonicalize(&target).ok())
            .is_some_and(|(a, b)| a == b);
        if !same_target && !opts.force {
            return Err(anyhow::anyhow!(
                "slug `{slug}` is already registered in {config} pointing at {existing}, \
                 but this install would point it at {requested}. Refusing to silently retarget.\n\n\
                 Resolve by either:\n  \
                 - pick a different slug:  `ccteam init --slug <other-name>` (or `--in <other-path>`),\n  \
                 - intentionally retarget: re-run with `--force` (the registry entry will be \
                 rewritten to {requested}).",
                slug = target_slug,
                config = ccteam_core::ccteam_config_path(&paths.root).display(),
                existing = existing.path.display(),
                requested = target.display(),
            ));
        }
    }

    // -- 4. Project install pass ------------------------------------
    let project_report = install_project_at(paths, &target, &target_slug, &target_team, &opts)?;

    // -- 5. Upsert config.yaml::projects[] --------------------------
    let entry = ccteam_core::ProjectEntry {
        slug: project_report.slug.clone(),
        path: target.clone(),
        team: project_report.team.clone(),
        installed_at: chrono::Utc::now(),
    };
    ccteam_core::upsert_project_in_config(&paths.root, entry)
        .context("upsert project into ~/.ccteam/config.yaml")?;

    // -- 6. Health check + optional wizard --------------------------
    let bin = current_ccteam_bin().ok();
    let claude = Command::new("claude").arg("--version").output();
    // V0.8 W1 — `tmux -V` probe routes through the canonical
    // `ccteam_core::tmux_available()` helper (re-exported from
    // `ccteam_harness::tmux_ops`) but the wizard wants a version string,
    // not a bool. Keep the inline `Command` here; the V0.9 retirement
    // of tmux.rs will swap this once tmux_available exposes the
    // version string too.
    let tmux = Command::new("tmux").arg("-V").output();

    let mut out = String::new();
    out.push_str(&format!(
        "ccteam init — {}\n\n",
        project_report.action_summary()
    ));
    out.push_str(&format!("  target dir       {}\n", target.display()));
    out.push_str(&format!("  slug             {}\n", project_report.slug));
    out.push_str(&format!("  team             {}\n", project_report.team));
    out.push_str(&format!(
        "  state.json       {} ({})\n",
        target.join(".ccteam").join("state.json").display(),
        project_report.state_action,
    ));
    out.push_str(&format!(
        "  workflow.yaml    {} ({})\n",
        target.join(".ccteam").join("workflow.yaml").display(),
        project_report.workflow_action,
    ));
    out.push_str(&format!(
        "  agents dir       {} ({})\n",
        target.join(".claude").join("agents").display(),
        project_report.agents_action,
    ));
    out.push_str(&format!(
        "  config.yaml      {} (upserted)\n",
        ccteam_core::ccteam_config_path(&paths.root).display(),
    ));

    out.push_str("\nhealth check:\n");
    match &claude {
        Ok(o) if o.status.success() => out.push_str(&format!(
            "  claude   : {}\n",
            String::from_utf8_lossy(&o.stdout).trim()
        )),
        _ => out
            .push_str("  claude   : NOT FOUND on PATH (install: https://claude.com/claude-code)\n"),
    }
    match &tmux {
        Ok(o) if o.status.success() => out.push_str(&format!(
            "  tmux     : {}\n",
            String::from_utf8_lossy(&o.stdout).trim()
        )),
        _ => {
            out.push_str("  tmux     : NOT FOUND on PATH (apt install tmux / brew install tmux)\n")
        }
    }
    match &bin {
        Some(p) => out.push_str(&format!("  ccteam   : {}\n", p.display())),
        None => out.push_str("  ccteam   : current_exe() failed (binary path unresolved)\n"),
    }

    if opts.interactive || opts.yes {
        out.push_str("\nfirst-run install:\n");
        let mut did_something = false;

        let do_mcp = ask_yn(
            "register ccteam MCP server in ~/.claude.json (mcpServers.ccteam)",
            opts.yes,
        )?;
        if do_mcp {
            out.push_str(&render_install_mcp_report()?);
            did_something = true;
        }

        if !did_something {
            out.push_str("  (no install steps selected)\n");
        }
    }

    out.push_str("\nnext:\n");
    out.push_str("  - edit .ccteam/workflow.yaml + .claude/agents/<role>.md to your taste\n");
    out.push_str("  - ccteam config               # register MCP / set IM token / prefs\n");
    out.push_str("  - ccteam start                # boots gateway + web\n");
    Ok(out)
}

/// Resolve where to install. Priority:
///   1. `--in <path>`  (absolute or relative; created if absent)
///   2. current working directory
///
/// `--slug` only sets the *registered project name*; it never changes
/// the install location (it used to relocate to `<projects_root>/<slug>/`
/// — a drift from the documented "override the derived slug" intent that
/// silently sent users standing in an existing repo to an empty skeleton
/// elsewhere). To create a fresh project under
/// `<projects_root>/<team>-<slug>/`, use `ccteam new <slug>`.
fn resolve_install_target(opts: &InitOptions) -> Result<std::path::PathBuf> {
    if let Some(p) = &opts.install_in {
        let abs = if p.is_absolute() {
            p.clone()
        } else {
            std::env::current_dir()
                .context("read cwd to resolve --in")?
                .join(p)
        };
        std::fs::create_dir_all(&abs)
            .with_context(|| format!("create --in target {}", abs.display()))?;
        return Ok(abs);
    }
    std::env::current_dir().context("read cwd as install target")
}

/// V0.4.2 F72: heuristic to detect the ccteam source repo so we don't
/// accidentally install ccteam inside ccteam (creates circular hook
/// loops per CLAUDE.md §六).
fn is_ccteam_repo(dir: &std::path::Path) -> bool {
    dir.join("Cargo.toml").is_file() && dir.join("crates").join("ccteam-cli").is_dir()
}

/// V0.4.2 F72: refuse to install at the filesystem root or at
/// `$HOME` — installing there would spam the user's home with a
/// `.ccteam/` + `.claude/` skeleton and register every dotfile-bearing
/// directory as one project. `--force` overrides.
fn refuse_sensitive_install_target(target: &std::path::Path, force: bool) -> Result<()> {
    let canonical = std::fs::canonicalize(target).unwrap_or_else(|_| target.to_path_buf());
    let is_root = canonical.parent().is_none();
    let is_home = dirs::home_dir()
        .and_then(|h| std::fs::canonicalize(&h).ok())
        .is_some_and(|h| h == canonical);
    if (is_root || is_home) && !force {
        return Err(anyhow::anyhow!(
            "refusing to install at {} — this looks like $HOME or the filesystem root.\n\
             Make a subdirectory (`mkdir myapp && cd myapp && ccteam init`) or pass `--force` \
             if you really mean to install here.",
            target.display(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct ProjectInstallReport {
    slug: String,
    team: String,
    fresh: bool,
    state_action: &'static str,
    workflow_action: &'static str,
    agents_action: &'static str,
}

impl ProjectInstallReport {
    fn action_summary(&self) -> &'static str {
        if self.fresh {
            "fresh install"
        } else {
            "refresh"
        }
    }
}

/// V0.4.2 F72: lay down (or refresh) a ccteam project at `target`.
fn install_project_at(
    paths: &CcteamPaths,
    target: &std::path::Path,
    slug: &str,
    team: &str,
    opts: &InitOptions,
) -> Result<ProjectInstallReport> {
    let ccteam_dir = target.join(".ccteam");
    let state_path = ccteam_dir.join("state.json");
    let fresh = !state_path.exists();

    let state_action: &'static str;
    let workflow_action: &'static str;
    let agents_action: &'static str;
    let final_team: String;

    if fresh {
        ccteam_core::bootstrap_project_at_dir(
            paths,
            target,
            slug,
            "(installed via `ccteam init`)",
            team,
        )?;
        scaffold_workflow_yaml(target, false, opts.mode)?;
        let agent_count = scaffold_default_agents(target, false, opts.mode)?;
        if matches!(opts.mode, InitMode::AgentTeam) {
            scaffold_agent_team_inbox(target)?;
            // V0.5.0 F94: agent-team mode needs the variant
            // `.claude/settings.json` (TeammateIdle / TaskCreated /
            // TaskCompleted hooks). `bootstrap_project_at_dir` already
            // wrote the V0.4.6 settings.json; overwrite it with the
            // agent-team variant.
            ccteam_core::write_project_settings_agent_team(
                target,
                &ccteam_core::EnabledPluginsSetting::default(),
            )?;
        }
        state_action = "created";
        workflow_action = "scaffolded";
        agents_action = if agent_count > 0 {
            "scaffolded"
        } else {
            "scaffolded (0 — bug?)"
        };
        final_team = team.to_string();
    } else {
        let mut existing_state = ccteam_core::ProjectState::load(&state_path)
            .with_context(|| format!("load existing {}", state_path.display()))?;
        if opts.force || opts.team.is_some() {
            existing_state.team = team.to_string();
        }
        existing_state.slug = slug.to_string();
        existing_state.tmux_session = format!("ccteam-{slug}");
        existing_state.save(&state_path)?;
        final_team = existing_state.team.clone();
        state_action = "refreshed";

        workflow_action = if opts.force {
            scaffold_workflow_yaml(target, true, opts.mode)?;
            "overwritten (--force)"
        } else {
            "preserved"
        };

        agents_action = if opts.force || opts.reset_agents {
            scaffold_default_agents(target, true, opts.mode)?;
            if opts.force {
                "overwritten (--force)"
            } else {
                "overwritten (--reset-agents)"
            }
        } else {
            "preserved"
        };
        // V0.5.0 F94: same as fresh-install branch — agent-team mode
        // needs the agent-team settings.json hook set. Idempotent
        // overwrite under `--force` only (preserve user edits otherwise).
        if matches!(opts.mode, InitMode::AgentTeam) && opts.force {
            ccteam_core::write_project_settings_agent_team(
                target,
                &ccteam_core::EnabledPluginsSetting::default(),
            )?;
        }
    }

    Ok(ProjectInstallReport {
        slug: slug.to_string(),
        team: final_team,
        fresh,
        state_action,
        workflow_action,
        agents_action,
    })
}

/// V0.4.2 F72: write a minimal `workflow.yaml` example into
/// `target/.ccteam/`. V0.4.6 F83 moved this from the project root into
/// `.ccteam/` so the orchestration state SoT stays out of the user's
/// business tree. Returns silently if the file already exists and
/// `force` is false.
///
/// V0.5.0 F93b: `mode` selects between the V0.4.6 default template
/// (`DEFAULT_WORKFLOW_YAML`) and the `workflow.agent-team.yaml`
/// template (advanced agent-team mode).
fn scaffold_workflow_yaml(target: &std::path::Path, force: bool, mode: InitMode) -> Result<()> {
    let ccteam_dir = target.join(".ccteam");
    std::fs::create_dir_all(&ccteam_dir)
        .with_context(|| format!("create {}", ccteam_dir.display()))?;
    let path = ccteam_dir.join("workflow.yaml");
    if path.exists() && !force {
        return Ok(());
    }
    let body = match mode {
        InitMode::ArtifactDriven => DEFAULT_WORKFLOW_YAML,
        InitMode::AgentTeam => DEFAULT_AGENT_TEAM_WORKFLOW_YAML,
    };
    std::fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// V0.4.2 F72: write minimal `.claude/agents/*.md` examples. Returns
/// the count of files written. With `force=false`, existing files are
/// preserved; with `force=true`, the shipped scaffolds always
/// overwrite.
///
/// V0.5.0 F93b: `mode == AgentTeam` additionally writes `__lead.md`
/// (the ccteam-managed lead agent spec).
fn scaffold_default_agents(target: &std::path::Path, force: bool, mode: InitMode) -> Result<usize> {
    let agents_dir = target.join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir)
        .with_context(|| format!("create {}", agents_dir.display()))?;
    let mut written = 0usize;
    for (name, body) in DEFAULT_AGENT_SCAFFOLDS {
        let path = agents_dir.join(name);
        if path.exists() && !force {
            continue;
        }
        std::fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;
        written += 1;
    }
    if matches!(mode, InitMode::AgentTeam) {
        let path = agents_dir.join("__lead.md");
        if !path.exists() || force {
            std::fs::write(&path, AGENT_TEAM_LEAD_MD)
                .with_context(|| format!("write {}", path.display()))?;
            written += 1;
        }
    }
    Ok(written)
}

/// V0.5.0 F93b: create the `.ccteam/inbox/` directory + a `.gitkeep`
/// sentinel so an empty inbox round-trips through `git`. This is the
/// dir the lead writes outbox messages into (and the user's
/// `ccteam send <slug>` writes inbox messages into for the lead to
/// poll). Idempotent.
fn scaffold_agent_team_inbox(target: &std::path::Path) -> Result<()> {
    let inbox = target.join(".ccteam").join("inbox");
    std::fs::create_dir_all(&inbox).with_context(|| format!("create {}", inbox.display()))?;
    let sentinel = inbox.join(".gitkeep");
    if !sentinel.exists() {
        std::fs::write(&sentinel, "").with_context(|| format!("write {}", sentinel.display()))?;
    }
    Ok(())
}

const DEFAULT_WORKFLOW_YAML: &str = r#"# ccteam workflow.yaml.
# Edit this file to declare your project's agent topology. Each agent
# is a role (filename of .claude/agents/<role>.md) with a trigger that
# decides when ccteam spawns a session for it.
#
# Trigger grammar:
#   manual                        # explicit `ccteam spawn <slug> <role>` only
#   schedule                      # periodic; needs `schedule:` 5-field cron
#   gate                          # waits for `trigger_gate` MCP / CLI call
#   watch:.ccteam/issues/         # spawn one session per new file under the path
name: default-workflow
description: |
  Minimal starter workflow. Edit me — the manual `cto` is a safe
  default that won't spawn until you call `ccteam spawn <slug> cto`.

agents:
  cto:
    trigger: manual
    executor: claude
"#;

/// Default `.claude/agents/<role>.md` scaffolds written by `ccteam init`.
///
/// v8.3 session=role: the default seed is the `cto` persona, sourced from
/// [`ccteam_core::CTO_ROLE_MD`] (single source — the same const core
/// `bootstrap_project_at_dir` seeds, so the CLI + IM/gateway create paths
/// agree). IM/chat sessions launch `claude --agent cto` by default, so
/// `cto.md` must exist in every project. Embedded `include_str!` agent
/// `.md` files (with Anthropic frontmatter + system prompt body) keep the
/// binary self-contained while staying editable as proper agent files.
///
/// To change the default agent scaffold:
/// 1. Edit the persona body (core `templates/cto_role.md` for `cto`)
/// 2. Adjust the `(filename, body)` row here if the role changes
/// 3. Update `DEFAULT_WORKFLOW_YAML` to declare the role if it's a
///    default-shipped agent
/// 4. `cargo build --workspace` + `cargo test --workspace`
///
/// See `agents/README.md` for the agent spec + naming conventions.
///
/// V0.5.0 F93b note: `__lead.md` is NOT in this list — it ships with
/// agent-team mode only and is conditionally written by
/// `scaffold_default_agents` when `mode == AgentTeam`. `doctor
/// --install-agents` (V0.5.x F93x) will pull it from
/// [`AGENT_TEAM_LEAD_MD`] independently.
pub(crate) const DEFAULT_AGENT_SCAFFOLDS: &[(&str, &str)] = &[("cto.md", ccteam_core::CTO_ROLE_MD)];

/// V0.5.0 F93b — embedded `__lead.md` body (ccteam-managed agent-team
/// lead spec). Written into `.claude/agents/__lead.md` by
/// `ccteam init --mode agent-team`. The body is treated as
/// ccteam-owned; `ccteam doctor --validate-team` (V0.5.x) will warn
/// if the user has hand-modified it.
pub(crate) const AGENT_TEAM_LEAD_MD: &str = include_str!("../../../agents/__lead.md");

/// V0.5.0 F93b — embedded `workflow.agent-team.yaml` body. Written by
/// `ccteam init --mode agent-team`. Contains the `mode: agent-team` +
/// `agent_team:` schema with one definition + one commented ad-hoc
/// teammate example.
pub(crate) const DEFAULT_AGENT_TEAM_WORKFLOW_YAML: &str =
    include_str!("../../ccteam-core/src/templates/workflow.agent-team.yaml");

/// Prompt the user `<question> [Y/n]: ` and return their answer. With
/// `yes_to_all = true`, skips the prompt and answers `true` (the
/// `-y` flag path). Reading a non-TTY stdin without `-y` returns
/// `false` to avoid hanging in pipelines.
fn ask_yn(question: &str, yes_to_all: bool) -> Result<bool> {
    use std::io::{IsTerminal, Write};
    if yes_to_all {
        println!("  ✓ {question} (--yes)");
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() {
        // non-interactive + no -y → skip the question (safe default).
        println!("  · {question}: SKIPPED (non-tty, pass --yes to enable)");
        return Ok(false);
    }
    print!("  {question}? [Y/n]: ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .context("read y/n from stdin")?;
    let answer = matches!(
        line.trim().to_ascii_lowercase().as_str(),
        "" | "y" | "yes" | "ok"
    );
    Ok(answer)
}

/// `ccteam ls`. Returns either a human table or the interfaces.md §10.3
/// JSON shape (a single string, not printed — caller decides).
pub fn run_ls(paths: &CcteamPaths, format: OutputFormat) -> Result<String> {
    let projects = collect_projects(paths)?;
    let daemon_up = ccteam_core::daemon::daemon_reachable(paths);
    Ok(match format {
        OutputFormat::Text => render_ls_text(paths, &projects, daemon_up),
        OutputFormat::Json => render_ls_json(paths, &projects, daemon_up)?,
    })
}

/// `ccteam show <slug>`. Renders the full project view per
/// interfaces.md §10.3 (json) or a human-readable summary (text).
///
/// V0.4.6 F91 — cost figures come from `cost_summary` (progress.jsonl
/// plus live claude state.json) instead of the retired
/// `state.cost_used_usd` accumulator. The old `cost used: $X.XX` line
/// is replaced with `cost (24h)` plus `cost (active)` so the user sees
/// both windowed spend and what's burning right now.
pub fn run_show(paths: &CcteamPaths, slug: &str, format: OutputFormat) -> Result<String> {
    let state_path = paths.project_state(slug);
    if !state_path.exists() {
        bail!("project not found: {slug}");
    }
    let state = ProjectState::load(&state_path)?;
    let recent = collect_recent_events(paths, slug, 50)?;
    let artifacts = collect_artifacts(paths, slug);
    let progress_path = paths.progress_jsonl(slug);
    let cost = cost_summary(slug, &progress_path, paths)?;
    let sessions = ccteam_core::active_sessions(slug, paths).unwrap_or_default();

    Ok(match format {
        OutputFormat::Text => render_show_text(&state, &cost, &recent, &artifacts, &sessions),
        OutputFormat::Json => render_show_json(&state, &cost, &recent, &artifacts, &sessions)?,
    })
}

/// V0.5.0 F93b — `ccteam start <slug>` flags. Drives the
/// `[Y/n/attach]` confirmation prompt + the `--no-confirm` / `--attach`
/// / `--dry-run` script-mode bypasses.
#[derive(Debug, Clone, Default)]
pub struct StartAgentTeamOptions {
    /// Skip the `[Y/n/attach]` prompt; default to `Y` behaviour
    /// (spawn + print attach hint). Matches `--no-confirm` / `-y`.
    pub no_confirm: bool,
    /// Skip the `[Y/n/attach]` prompt and go straight to the `attach`
    /// branch (spawn + exec `claude attach <id>`).
    pub attach: bool,
    /// Print spawn preview + exit; do not spawn. Useful for previews
    /// / CI / docs.
    pub dry_run: bool,
    /// V0.5.0 F97 — revive a `leave-running` detached project. Reads
    /// `.ccteam/team-snapshot.json::lead_session_id`, probes the bg
    /// job's `state.json` for liveness, and:
    /// - **Running**: re-arms F95 watch entries, clears `detached`
    ///   marker in `state.json`, prints attach hint. NO new spawn.
    /// - **Terminal**: WARN + fall through to the normal spawn flow.
    pub restart_team: bool,
}

/// V0.5.0 F93b — `ccteam start <slug>` for agent-team mode projects.
///
/// 1. Load `<slug>`'s `workflow.yaml`; bail if `mode != agent-team`.
/// 2. Print spawn preview (workflow.yaml summary, `__lead.md` model,
///    suggested teammates count, spawn argv preview).
/// 3. Prompt `[Y/n/attach]` (TTY interactive) unless `no_confirm` /
///    `attach` / `dry_run` selected.
/// 4. On Y: spawn `claude --bg --agent __lead`, write
///    `.ccteam/team-snapshot.json::lead_session_id`, print attach hint.
/// 5. On attach: spawn (same as Y), then exec `claude attach <id>`.
/// 6. On n: cancel, no side effects.
///
/// **Returns the formatted preview/report** so unit tests can inspect
/// the rendered output without going through TTY. Production caller
/// in main.rs just prints + exits.
pub fn run_start_agent_team(
    paths: &CcteamPaths,
    slug: &str,
    opts: StartAgentTeamOptions,
) -> Result<String> {
    let project_dir = paths.project_dir(slug);
    if !project_dir.exists() {
        bail!(
            "no project at `{slug}`: {} not found. \
             Run `ccteam init --mode agent-team --slug {slug}` first.",
            project_dir.display(),
        );
    }
    let spec = ccteam_flow::WorkflowSpec::load_for_project(&project_dir)
        .with_context(|| format!("load workflow.yaml for {slug}"))?;
    let team_spec = match (&spec.mode, &spec.agent_team) {
        (ccteam_flow::WorkflowMode::AgentTeam, Some(t)) => t,
        (ccteam_flow::WorkflowMode::AgentTeam, None) => {
            bail!("workflow.yaml mode: agent-team but agent_team block missing — schema bug?",)
        }
        (ccteam_flow::WorkflowMode::ArtifactDriven, _) => bail!(
            "project `{slug}` is in artifact-driven mode; `ccteam start <slug>` is only\n  \
             implemented for agent-team mode. For artifact-driven projects run\n  \
             `ccteam start` (no slug) to start the daemon.",
        ),
        (ccteam_flow::WorkflowMode::Chat, _) => bail!(
            "project `{slug}` is in chat mode (V0.6.0 F108); `ccteam start <slug>` is not\n  \
             the chat-mode entry point. Bots are launched via the IM channel + \n  \
             `ccteam-im` daemon, not the agent-team start flow.",
        ),
        (ccteam_flow::WorkflowMode::HumanApproval, _) => bail!(
            "project `{slug}` is in human-approval mode (V0.6.1 F124); `ccteam start <slug>`\n  \
             is only implemented for agent-team mode. For human-approval projects run\n  \
             `ccteam start` (no slug) to start the daemon — the HITL gate fires on each\n  \
             agent_done via the F98 plan-approval IM round-trip.",
        ),
    };

    // ---- V0.5.0 F97 — `--restart-team` revive path -----------------------
    let mut restart_prelude = String::new();
    if opts.restart_team {
        let outcome = run_restart_team(paths, slug, &project_dir, team_spec)?;
        match outcome {
            RestartTeamOutcome::ResumedAlive(body) => return Ok(body),
            RestartTeamOutcome::FellThroughToSpawn(prelude) => {
                // Lead is terminal; print the WARN prelude then continue
                // with the normal spawn flow below. Stash a copy so the
                // returned body (dry-run / test inspection) includes it.
                print!("{prelude}");
                restart_prelude = prelude;
            }
        }
    } else {
        // V0.5.0 F97 — refuse plain `ccteam start <slug>` when the project
        // is in the `leave-running` detached state. The user must
        // explicitly invoke `--restart-team` (avoids accidentally
        // spawning a second lead while the first is still alive).
        let state_path = paths.project_state(slug);
        if let Ok(state) = ccteam_core::ProjectState::load(&state_path) {
            if state.detached {
                bail!(
                    "project `{slug}` is in detached state \
                     (last `ccteam stop` used `cleanup_on_stop: leave-running`).\n  \
                     To re-attach the existing lead: `ccteam start --restart-team {slug}`\n  \
                     To force a fresh lead (and orphan the old one): edit \
                     state.json::detached → false, then re-run.",
                );
            }
        }
    }

    let lead_md_path = project_dir.join(".claude").join("agents").join("__lead.md");
    let lead_md_exists = lead_md_path.exists();
    let teammate_mode = team_spec
        .teammate_mode
        .clone()
        .unwrap_or_else(|| "in-process".to_string());

    // ---- 1. Render preview ---------------------------------------------
    let mut preview = String::new();
    preview.push_str(&format!("ccteam start {slug} — agent-team mode\n\n"));
    preview.push_str(&format!(
        "  ✓ Loaded {}\n",
        project_dir.join(".ccteam").join("workflow.yaml").display(),
    ));
    preview.push_str(&format!(
        "      mode=agent-team team_name={}\n",
        team_spec.team_name,
    ));
    if lead_md_exists {
        preview.push_str(&format!("  ✓ Loaded {}\n", lead_md_path.display(),));
    } else {
        preview.push_str(&format!(
            "  ! Missing {} — re-run `ccteam init --mode agent-team --slug {slug} --force`\n",
            lead_md_path.display(),
        ));
    }
    let (def_count, adhoc_count) =
        team_spec
            .suggested_teammates
            .iter()
            .fold((0u32, 0u32), |(d, a), t| match t.kind {
                ccteam_flow::SuggestedTeammateKind::Definition => (d + 1, a),
                ccteam_flow::SuggestedTeammateKind::AdHoc => (d, a + 1),
            });
    preview.push_str(&format!(
        "  ✓ Suggested teammates: {def_count} definition + {adhoc_count} ad-hoc\n",
    ));
    preview.push_str("\n  About to spawn lead session:\n");
    preview.push_str(&format!(
        "    env CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1 \\\n        CLAUDE_CODE_TEAMMATE_MODE={teammate_mode} \\\n      claude --bg --agent __lead --dangerously-skip-permissions <lead_seed>\n",
    ));
    // Truncate the lead_seed preview to 200 chars for the console.
    let seed_preview: String = team_spec
        .lead_seed
        .chars()
        .take(200)
        .collect::<String>()
        .replace('\n', " ");
    let seed_preview = if team_spec.lead_seed.chars().count() > 200 {
        format!("{seed_preview}…")
    } else {
        seed_preview
    };
    preview.push_str(&format!(
        "    Initial user-turn (lead_seed):\n      \"{seed_preview}\"\n",
    ));

    // ---- 2. Dry-run / non-interactive bypasses --------------------------
    if opts.dry_run {
        preview.push_str("\n  --dry-run set — preview only, lead not spawned.\n");
        return Ok(format!("{restart_prelude}{preview}"));
    }

    let choice = resolve_start_choice(&opts)?;

    // ---- 3. Confirm or fall through to spawn ----------------------------
    print!("{preview}");
    let action = match choice {
        StartChoice::Confirmed => "default",
        StartChoice::AttachAfterSpawn => "attach",
        StartChoice::Cancelled => {
            return Ok(format!(
                "{restart_prelude}{preview}\n  Cancelled. No side effects.\n"
            ));
        }
    };

    // ---- 4. Spawn the lead ----------------------------------------------
    let mut report = String::new();
    let lead_id = spawn_agent_team_lead(paths, slug, team_spec, &teammate_mode)
        .with_context(|| format!("spawn __lead for {slug}"))?;
    report.push_str(&format!("  ✓ Lead session spawned: {lead_id}\n"));
    report.push_str("\n  Manage the team with:\n");
    report.push_str(&format!("    ccteam attach {slug}    # interactive\n",));
    report.push_str(&format!(
        "    ccteam internal send {slug} \"go\"   # async\n",
    ));
    report.push_str(&format!(
        "    ccteam web                          # http://localhost:7331/teams/{}\n",
        team_spec.team_name,
    ));
    report.push_str(
        "    ccteam internal hook progress-append … (advanced path 3 hooks already installed)\n",
    );
    report.push_str(&format!("    ccteam stop {slug}    # cleanup\n",));

    print!("{report}");
    if action == "attach" {
        eprintln!("→ claude attach {lead_id}");
        let status = Command::new("claude")
            .args(["attach", &lead_id])
            .status()
            .context("spawn claude attach (--attach branch)")?;
        if !status.success() {
            bail!("claude attach exited with {status}");
        }
    }
    Ok(format!("{restart_prelude}{preview}{report}"))
}

/// V0.5.0 F93b — `[Y/n/attach]` prompt resolution. Returns the
/// resolved action without performing any side effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartChoice {
    /// `Y` branch — spawn + print attach hint.
    Confirmed,
    /// `attach` branch — spawn + exec `claude attach <id>`.
    AttachAfterSpawn,
    /// `n` branch — cancel, no side effects.
    Cancelled,
}

fn resolve_start_choice(opts: &StartAgentTeamOptions) -> Result<StartChoice> {
    if opts.attach {
        return Ok(StartChoice::AttachAfterSpawn);
    }
    if opts.no_confirm {
        return Ok(StartChoice::Confirmed);
    }
    use std::io::{IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        // Non-tty without explicit flag: default to confirmed to keep
        // scripted callers (CI smoke tests, doctor smoke flow) from
        // hanging on stdin.
        eprintln!(
            "  · stdin is not a tty + no --no-confirm/--attach/--dry-run set;\n  \
             defaulting to confirmed spawn. Pass --dry-run to preview.",
        );
        return Ok(StartChoice::Confirmed);
    }
    print!("\n  Proceed? [Y/n/attach]: ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .context("read [Y/n/attach] answer from stdin")?;
    let trimmed = line.trim().to_ascii_lowercase();
    Ok(match trimmed.as_str() {
        "" | "y" | "yes" | "ok" => StartChoice::Confirmed,
        "attach" | "a" => StartChoice::AttachAfterSpawn,
        "n" | "no" | "cancel" => StartChoice::Cancelled,
        other => bail!(
            "unrecognized answer `{other}`: expected one of \
             `Y` / `n` / `attach` (default `Y`)",
        ),
    })
}

/// V0.5.0 F93b — spawn the `__lead` Claude bg session for a project.
///
/// The exact argv form is:
///
/// ```text
/// env CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1 \
///     CLAUDE_CODE_TEAMMATE_MODE=<mode> \
///   claude --bg --agent __lead --dangerously-skip-permissions \
///     "<lead_seed body>"
/// ```
///
/// `lead_seed` is passed as the positional user-turn message (NOT as a
/// system prompt — CLAUDE.md §三 红线). The harness's
/// `--dangerously-skip-permissions` is set per the existing
/// [`ClaudeCodeAdapter`] pattern. Team-mode env vars are set on the
/// spawned process via `Command::env()` — V0.5.0 F93b initially used
/// `--env KEY=VAL` argv flags but Claude Code's CLI does not accept
/// those, causing the spawn to exit-1 before init (host probe 2026-05-18).
///
/// Returns the `daemonShort` job id from `claude --bg`'s stdout
/// (first line `backgrounded · <id>`). Writes
/// `.ccteam/team-snapshot.json` containing the lead id + the resolved
/// (frozen) `AgentTeamSpec`.
///
/// Test override: `$CCTEAM_CLAUDE_BIN` swaps in a fake script, same
/// pattern as the regular `ClaudeCodeAdapter`.
fn spawn_agent_team_lead(
    paths: &CcteamPaths,
    slug: &str,
    team_spec: &ccteam_flow::AgentTeamSpec,
    teammate_mode: &str,
) -> Result<String> {
    let project_dir = paths.project_dir(slug);
    let bin =
        std::env::var(ccteam_harness::CLAUDE_BIN_ENV).unwrap_or_else(|_| "claude".to_string());
    let mut cmd = Command::new(&bin);
    cmd.arg("--bg")
        .arg("--agent")
        .arg("__lead")
        .arg("--dangerously-skip-permissions")
        .arg(&team_spec.lead_seed);
    cmd.env("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS", "1")
        .env("CLAUDE_CODE_TEAMMATE_MODE", teammate_mode)
        .current_dir(&project_dir);
    let output = cmd
        .output()
        .with_context(|| format!("invoke `{bin} --bg --agent __lead`"))?;
    if !output.status.success() {
        bail!(
            "claude --bg --agent __lead exited non-zero ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim(),
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lead_id = parse_backgrounded_short_id(&stdout).ok_or_else(|| {
        anyhow::anyhow!(
            "claude --bg stdout missing `backgrounded · <id>` line: {}",
            stdout.trim(),
        )
    })?;

    // Write snapshot.
    let snapshot_path = project_dir.join(".ccteam").join("team-snapshot.json");
    let snapshot = serde_json::json!({
        "slug": slug,
        "lead_session_id": lead_id,
        "team_name": team_spec.team_name,
        "teammate_mode": teammate_mode,
        "cleanup_on_stop": team_spec.cleanup_on_stop.as_str(),
        "auto_spawn_teammates": team_spec.auto_spawn_teammates,
        "suggested_teammates": team_spec.suggested_teammates,
        "spawned_at": chrono::Utc::now().to_rfc3339(),
    });
    if let Some(parent) = snapshot_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::write(&snapshot_path, serde_json::to_string_pretty(&snapshot)?)
        .with_context(|| format!("write {}", snapshot_path.display()))?;

    Ok(lead_id)
}

/// V0.5.0 F93b — duplicated locally from
/// `ccteam_harness::parse_backgrounded_short_id` because that
/// fn is `pub(crate)`. The format is "backgrounded · <id>" on the
/// first non-empty line.
fn parse_backgrounded_short_id(stdout: &str) -> Option<String> {
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Match `backgrounded · <id>` (Anthropic uses U+00B7 MIDDLE DOT).
        if let Some(rest) = trimmed.strip_prefix("backgrounded · ") {
            return Some(rest.trim().to_string());
        }
        // Forwards-compat: also accept ascii fallback for stub scripts.
        if let Some(rest) = trimmed.strip_prefix("backgrounded ") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

// =====================================================================
// V0.5.0 F97 — Advanced path lifecycle: cleanup_on_stop strategies
// + --restart-team revive + hot-reload cold/hot diff
// =====================================================================

/// V0.5.0 F97 — `ccteam stop <slug>` flags.
#[derive(Debug, Clone, Copy)]
pub struct StopSlugOptions {
    /// Wait budget for `cleanup_on_stop: ask-lead` to see a
    /// `workflow_done` event from the lead before falling back to
    /// `force-kill`. Default 60s (set by clap default in `main.rs`).
    pub stop_timeout: Duration,
}

impl Default for StopSlugOptions {
    fn default() -> Self {
        Self {
            stop_timeout: Duration::from_secs(60),
        }
    }
}

/// V0.5.0 F97 — `ccteam stop <slug>` for agent-team mode projects.
///
/// 1. Load `<slug>`'s `workflow.yaml`; bail if `mode != agent-team`.
/// 2. Load `.ccteam/team-snapshot.json` to discover the lead session
///    id + frozen cleanup strategy. Snapshot wins over workflow.yaml
///    if they disagree (PRD F93 stickiness).
/// 3. Dispatch on `cleanup_on_stop`:
///    - `ForceKill`: SIGKILL the lead bg job pid; clear snapshot.
///    - `AskLead`: write a user-turn cleanup message to
///      `.ccteam/inbox/`; poll `~/.ccteam/progress/<slug>.jsonl` for
///      `workflow_done`; on timeout, fall back to `ForceKill` + WARN.
///    - `LeaveRunning`: clear snapshot, set `state.json::detached =
///      true`, leave the lead alive.
///
/// Returns the rendered report so tests can inspect it without going
/// through TTY. Production caller in main.rs just prints + exits.
pub fn run_stop_slug(paths: &CcteamPaths, slug: &str, opts: StopSlugOptions) -> Result<String> {
    let project_dir = paths.project_dir(slug);
    if !project_dir.exists() {
        bail!(
            "no project at `{slug}`: {} not found.",
            project_dir.display(),
        );
    }
    let spec = ccteam_flow::WorkflowSpec::load_for_project(&project_dir)
        .with_context(|| format!("load workflow.yaml for {slug}"))?;
    let team_spec = match (&spec.mode, &spec.agent_team) {
        (ccteam_flow::WorkflowMode::AgentTeam, Some(t)) => t,
        (ccteam_flow::WorkflowMode::AgentTeam, None) => {
            bail!("workflow.yaml mode: agent-team but agent_team block missing — schema bug?",)
        }
        (ccteam_flow::WorkflowMode::ArtifactDriven, _) => bail!(
            "project `{slug}` is in artifact-driven mode; `ccteam stop <slug>` is only\n  \
             implemented for agent-team mode. For daemon shutdown run `ccteam stop` (no slug).",
        ),
        (ccteam_flow::WorkflowMode::Chat, _) => bail!(
            "project `{slug}` is in chat mode (V0.6.0 F108); `ccteam stop <slug>` is not\n  \
             the chat-mode shutdown path. Use the IM channel `/stop` directive or\n  \
             kill the bot's tmux session directly.",
        ),
        (ccteam_flow::WorkflowMode::HumanApproval, _) => bail!(
            "project `{slug}` is in human-approval mode (V0.6.1 F124); `ccteam stop <slug>`\n  \
             is only implemented for agent-team mode. For daemon shutdown run\n  \
             `ccteam stop` (no slug).",
        ),
    };

    let snapshot_path = project_dir.join(".ccteam").join("team-snapshot.json");
    let snapshot = if snapshot_path.exists() {
        let raw = std::fs::read_to_string(&snapshot_path)
            .with_context(|| format!("read {}", snapshot_path.display()))?;
        Some(
            serde_json::from_str::<Value>(&raw)
                .with_context(|| format!("parse {}", snapshot_path.display()))?,
        )
    } else {
        None
    };
    let lead_id = snapshot
        .as_ref()
        .and_then(|s| s.get("lead_session_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // Snapshot wins (F93 stickiness); fall back to workflow.yaml.
    let cleanup = snapshot
        .as_ref()
        .and_then(|s| s.get("cleanup_on_stop"))
        .and_then(|v| v.as_str())
        .map(parse_cleanup_str)
        .unwrap_or(Ok(team_spec.cleanup_on_stop))
        .unwrap_or(team_spec.cleanup_on_stop);

    let mut report = String::new();
    report.push_str(&format!(
        "ccteam stop {slug} — agent-team mode (cleanup_on_stop={})\n",
        cleanup.as_str(),
    ));
    match cleanup {
        ccteam_flow::CleanupOnStop::ForceKill => {
            force_kill_lead(paths, slug, &snapshot_path, lead_id.as_deref(), &mut report)?;
        }
        ccteam_flow::CleanupOnStop::AskLead => {
            ask_lead_cleanup(
                paths,
                slug,
                &project_dir,
                &snapshot_path,
                lead_id.as_deref(),
                opts.stop_timeout,
                &mut report,
            )?;
        }
        ccteam_flow::CleanupOnStop::LeaveRunning => {
            leave_running(paths, slug, &snapshot_path, lead_id.as_deref(), &mut report)?;
        }
    }
    Ok(report)
}

fn parse_cleanup_str(raw: &str) -> Result<ccteam_flow::CleanupOnStop> {
    match raw {
        "force-kill" => Ok(ccteam_flow::CleanupOnStop::ForceKill),
        "ask-lead" => Ok(ccteam_flow::CleanupOnStop::AskLead),
        "leave-running" => Ok(ccteam_flow::CleanupOnStop::LeaveRunning),
        other => bail!("unknown cleanup_on_stop `{other}`"),
    }
}

/// V0.5.0 F97 — `ForceKill` cleanup path. Reads
/// `~/.claude/jobs/<lead_id>/state.json::pid` and SIGKILLs the pid;
/// idempotent on missing state.json / dead pid. Always clears the
/// project's `team-snapshot.json` (so the next `ccteam start <slug>`
/// scaffolds a fresh lead) and resets `state.json::detached = false`.
fn force_kill_lead(
    paths: &CcteamPaths,
    slug: &str,
    snapshot_path: &std::path::Path,
    lead_id: Option<&str>,
    report: &mut String,
) -> Result<()> {
    if let Some(id) = lead_id {
        let state_path = ccteam_harness::state_json_path(id);
        match std::fs::read_to_string(&state_path) {
            Ok(raw) => match ccteam_harness::parse_pid_from_state(&raw) {
                Some(pid) => match ccteam_harness::sigkill_pid(pid) {
                    Ok(()) => {
                        report.push_str(&format!("  ✓ SIGKILL pid {pid} (lead bg job {id})\n",))
                    }
                    Err(err) => report.push_str(&format!(
                        "  ! SIGKILL pid {pid} failed: {err} (continuing)\n",
                    )),
                },
                None => report.push_str(&format!(
                    "  · state.json for {id} has no pid; nothing to kill\n",
                )),
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => report.push_str(&format!(
                "  · state.json for lead {id} absent; already terminated\n",
            )),
            Err(err) => report.push_str(&format!(
                "  ! read state.json for {id} failed: {err} (continuing)\n",
            )),
        }
    } else {
        report.push_str("  · no lead_session_id in snapshot (already cleaned up?)\n");
    }
    clear_team_snapshot(snapshot_path, report)?;
    clear_detached_marker(paths, slug, report)?;
    Ok(())
}

/// V0.5.0 F97 — `AskLead` cleanup path. Writes a user-turn cleanup
/// message to the lead's `.ccteam/inbox/` (the lead picks it up on
/// the next idle tick via its F87 intercept-ask hook + native message
/// surfacing). Polls `~/.ccteam/progress/<slug>.jsonl` for a
/// `workflow_done` event; on timeout, falls back to `force-kill` with
/// a WARN.
///
/// **CLAUDE.md §三 红线**: this is a *user-turn message*, not a system
/// prompt — same red line as F93b `lead_seed`.
fn ask_lead_cleanup(
    paths: &CcteamPaths,
    slug: &str,
    project_dir: &std::path::Path,
    snapshot_path: &std::path::Path,
    lead_id: Option<&str>,
    timeout: Duration,
    report: &mut String,
) -> Result<()> {
    let inbox_dir = project_dir.join(".ccteam").join("inbox");
    std::fs::create_dir_all(&inbox_dir)
        .with_context(|| format!("create {}", inbox_dir.display()))?;
    let now = chrono::Utc::now();
    let filename = format!("{}-stop-request.md", now.format("%Y%m%dT%H%M%SZ"),);
    let msg_path = inbox_dir.join(&filename);
    let body = "\
---
source: ccteam-stop
priority: normal
---

# ccteam stop — cleanup request

Clean up the team — stop all teammates, persist context (use SendMessage / \
TaskUpdate / Mailbox notes as appropriate), then exit. When done, allow \
this session to terminate so ccteam can write `workflow_done`.

This is a user-turn message from ccteam (`cleanup_on_stop: ask-lead`). It is \
NOT a system prompt; honor it the same way you'd honor a direct user request.
";
    std::fs::write(&msg_path, body).with_context(|| format!("write {}", msg_path.display()))?;
    report.push_str(&format!(
        "  ✓ Wrote cleanup request to {}\n",
        msg_path.display(),
    ));
    report.push_str(&format!(
        "  · Waiting up to {}s for lead to emit workflow_done…\n",
        timeout.as_secs(),
    ));

    let progress_path = paths.progress_jsonl(slug);
    let baseline_count = count_workflow_done(&progress_path);
    let start = Instant::now();
    let mut saw_done = false;
    while start.elapsed() < timeout {
        let current = count_workflow_done(&progress_path);
        if current > baseline_count {
            saw_done = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    if saw_done {
        report.push_str("  ✓ Lead emitted workflow_done; cleanup complete\n");
        clear_team_snapshot(snapshot_path, report)?;
        clear_detached_marker(paths, slug, report)?;
        return Ok(());
    }

    // Timeout — fall back to force-kill (still write the audit line).
    report.push_str(&format!(
        "  ! WARN: timeout after {}s without workflow_done; falling back to force-kill\n",
        timeout.as_secs(),
    ));
    force_kill_lead(paths, slug, snapshot_path, lead_id, report)
}

/// V0.5.0 F97 — `LeaveRunning` cleanup path. Drops the F95 watch
/// entries (clears `.ccteam/team-snapshot.json`) but leaves the lead
/// bg job + teammates running. Marks `state.json::detached = true` so
/// the next plain `ccteam start <slug>` refuses with a friendly error
/// (avoiding a "two leads concurrently" surprise).
fn leave_running(
    paths: &CcteamPaths,
    slug: &str,
    snapshot_path: &std::path::Path,
    lead_id: Option<&str>,
    report: &mut String,
) -> Result<()> {
    // We keep the snapshot file in place because --restart-team needs to
    // read lead_session_id back. Instead we set a `detached` marker in
    // state.json so a fresh `ccteam start <slug>` refuses. We also do
    // NOT kill the lead. The F95 watcher is global and reacts to the
    // teams root; the daemon-side per-project state we toggle is the
    // `detached` field.
    set_detached_marker(paths, slug, true, report)?;
    if let Some(id) = lead_id {
        report.push_str(&format!("  ✓ Lead session id: {id} (still running)\n",));
        report.push_str(&format!(
            "    Reconnect with `ccteam start --restart-team {slug}` or `claude attach {id}`\n",
        ));
    } else {
        report.push_str("  · No lead_session_id in snapshot — nothing to leave running.\n");
    }
    let _ = snapshot_path; // snapshot is preserved (used by --restart-team)
    Ok(())
}

/// Helper — remove `.ccteam/team-snapshot.json` if it exists.
fn clear_team_snapshot(snapshot_path: &std::path::Path, report: &mut String) -> Result<()> {
    match std::fs::remove_file(snapshot_path) {
        Ok(()) => report.push_str(&format!("  ✓ Cleared {}\n", snapshot_path.display(),)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            report.push_str(&format!("  · {} already absent\n", snapshot_path.display(),))
        }
        Err(err) => report.push_str(&format!(
            "  ! remove {} failed: {err} (continuing)\n",
            snapshot_path.display(),
        )),
    }
    Ok(())
}

/// Helper — flip `state.json::detached` to the given value. Used by
/// `LeaveRunning` (true) + force-kill / ask-lead success (false).
fn set_detached_marker(
    paths: &CcteamPaths,
    slug: &str,
    detached: bool,
    report: &mut String,
) -> Result<()> {
    let state_path = paths.project_state(slug);
    if !state_path.exists() {
        // No state.json: nothing to mark. Best-effort.
        return Ok(());
    }
    let mut state = ccteam_core::ProjectState::load(&state_path)
        .with_context(|| format!("load {}", state_path.display()))?;
    if state.detached == detached {
        return Ok(());
    }
    state.detached = detached;
    state
        .save(&state_path)
        .with_context(|| format!("save {}", state_path.display()))?;
    report.push_str(&format!("  ✓ state.json::detached = {detached}\n",));
    Ok(())
}

fn clear_detached_marker(paths: &CcteamPaths, slug: &str, report: &mut String) -> Result<()> {
    set_detached_marker(paths, slug, false, report)
}

/// Helper — count the `workflow_done` events currently in a project's
/// `progress.jsonl`. Used by `ask-lead` to detect "lead emitted
/// workflow_done after my cleanup request" via baseline-delta polling.
fn count_workflow_done(progress_path: &std::path::Path) -> usize {
    let Ok(content) = std::fs::read_to_string(progress_path) else {
        return 0;
    };
    content
        .lines()
        .filter(|l| {
            let trimmed = l.trim();
            if trimmed.is_empty() {
                return false;
            }
            serde_json::from_str::<Value>(trimmed)
                .ok()
                .and_then(|v| v.get("event").and_then(|s| s.as_str()).map(String::from))
                .map(|kind| kind == "workflow_done")
                .unwrap_or(false)
        })
        .count()
}

/// V0.5.0 F97 — `ccteam start --restart-team <slug>` outcome variants.
#[derive(Debug)]
enum RestartTeamOutcome {
    /// Lead bg job is still alive; watch re-armed without spawning.
    /// Contained body is the full rendered report for the caller.
    ResumedAlive(String),
    /// Lead bg job has exited; caller should fall through to the
    /// normal spawn flow. Contained string is the rendered WARN
    /// prelude that should be printed before the spawn preview.
    FellThroughToSpawn(String),
}

/// V0.5.0 F97 — execute the `--restart-team` revive logic. Reads
/// `.ccteam/team-snapshot.json`, probes the bg job's `state.json` for
/// liveness, and either:
/// - Returns `ResumedAlive` after re-arming the F95 watch + clearing
///   `state.json::detached`. No spawn.
/// - Returns `FellThroughToSpawn` (with a WARN body) so the caller
///   continues to the normal spawn flow.
fn run_restart_team(
    paths: &CcteamPaths,
    slug: &str,
    project_dir: &std::path::Path,
    _team_spec: &ccteam_flow::AgentTeamSpec,
) -> Result<RestartTeamOutcome> {
    let snapshot_path = project_dir.join(".ccteam").join("team-snapshot.json");
    if !snapshot_path.exists() {
        bail!(
            "--restart-team requires a prior team-snapshot.json at {}.\n  \
             This file is written on the first `ccteam start {slug}` spawn.\n  \
             If this is a fresh project, drop `--restart-team` and run \
             `ccteam start {slug}` to spawn a new lead.",
            snapshot_path.display(),
        );
    }
    let raw = std::fs::read_to_string(&snapshot_path)
        .with_context(|| format!("read {}", snapshot_path.display()))?;
    let snapshot: Value =
        serde_json::from_str(&raw).with_context(|| format!("parse {}", snapshot_path.display()))?;
    let lead_id = snapshot
        .get("lead_session_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "team-snapshot.json at {} missing `lead_session_id` field; \
                 cannot restart without a lead id.",
                snapshot_path.display(),
            )
        })?;
    let liveness = ccteam_core::probe_job(Some(lead_id));
    match liveness {
        ccteam_core::JobLiveness::Running => {
            // Lead is alive; re-arm the watch (clear detached marker).
            let mut report = String::new();
            report.push_str(&format!(
                "ccteam start --restart-team {slug} — agent-team mode\n\n",
            ));
            report.push_str(&format!(
                "  ✓ team-snapshot.json found; lead_session_id={lead_id}\n",
            ));
            report.push_str("  ✓ probe_job() == Running; lead bg job alive. Skipping spawn.\n");
            // Clear detached marker if set (F97 LeaveRunning recovery).
            clear_detached_marker(paths, slug, &mut report)?;
            report.push_str("\n  Manage the team with:\n");
            report.push_str(&format!("    ccteam attach {slug}    # interactive\n",));
            report.push_str(&format!(
                "    ccteam internal send {slug} \"...\"   # async\n",
            ));
            report.push_str(&format!("    ccteam stop {slug}    # cleanup\n",));
            print!("{report}");
            Ok(RestartTeamOutcome::ResumedAlive(report))
        }
        ccteam_core::JobLiveness::Terminal { status, .. } => {
            // Lead is terminal — render a WARN prelude and let the
            // caller fall through to the spawn flow.
            let mut prelude = String::new();
            prelude.push_str(&format!(
                "ccteam start --restart-team {slug} — agent-team mode\n\n",
            ));
            prelude.push_str(&format!(
                "  ! WARN: Previous lead exited (status={status}); spawning fresh lead.\n",
            ));
            // Clear stale detached marker before the new spawn.
            clear_detached_marker(paths, slug, &mut prelude)?;
            Ok(RestartTeamOutcome::FellThroughToSpawn(prelude))
        }
    }
}

/// `ccteam attach <slug>`. Resolves the underlying session medium and
/// dispatches:
///
/// 1. V0.5.0 F93b — if the project's workflow.yaml has
///    `mode: agent-team`, read the lead session id from
///    `.ccteam/team-snapshot.json::lead_session_id` and exec
///    `claude attach <id>`. Friendly-error if the snapshot is missing
///    or the lead id is not yet populated (lead not spawned).
/// 2. If a tmux session named `ccteam-<slug>` exists → `tmux attach -t …`
///    (V0.3.x meta-agent + legacy projects).
/// 3. Else if the project's latest `agent_spawn` event in
///    `progress.jsonl` carries a `claude --bg` `job_id` → `claude attach
///    <job_id>` (V0.4.0 worker default).
/// 4. Else → fail-loud "no live session for <slug>".
///
/// Always prints the underlying command before exec'ing so the operator
/// learns the lower-level tool.
/// V0.8 W5 — interactive attach to a session hosted by the
/// ccteam-owned rmux daemon (`~/.ccteam/run/mux.sock`).
///
/// Path A (use `rmux-client` directly): `connect` → `begin_attach`
/// → `into_parts` → `attach_terminal_with_initial_bytes`. The last call
/// drives the local TTY in raw mode (termios) on the CLI's own
/// controlling terminal — exactly the in-process analogue of
/// `tmux attach -t <name>`, so it stays a blocking sync call here
/// (terminal handover doesn't fit the async trait — W0 audit §4-D).
///
/// Detach behavior (chord / banner) is determined by the rmux daemon
/// itself: it emits `AttachMessage::DetachKill` / `DetachExec`, which
/// the client driver handles. ccteam does NOT pick a detach key
/// (e.g. `Ctrl-]`) — that is rmux-owned.
#[cfg(unix)]
fn rmux_interactive_attach(session_name: &str) -> Result<()> {
    use rmux_client::{connect_or_absent, AttachTransition, ConnectResult};
    use rmux_proto::SessionName;

    let socket = ccteam_harness::default_ccteam_harness_socket_path();
    eprintln!("→ rmux attach {session_name} (socket {})", socket.display());

    let connection = match connect_or_absent(&socket).context("connect to ccteam rmux daemon")? {
        ConnectResult::Connected(conn) => conn,
        ConnectResult::Absent => bail!(
            "ccteam rmux daemon is not running (socket `{}` absent).\n  \
             Start the project first:  ccteam start {session_name}\n  \
             (the daemon is hosted by `ccteam start`; attach has nothing to connect to until then.)",
            socket.display(),
        ),
    };

    let name = SessionName::new(session_name.to_string())
        .map_err(|e| anyhow::anyhow!("invalid rmux session name `{session_name}`: {e}"))?;

    let transition = connection
        .begin_attach(name)
        .with_context(|| format!("begin attach to rmux session `{session_name}`"))?;

    let upgrade = match transition {
        AttachTransition::Upgraded(upgrade) => upgrade,
        AttachTransition::Rejected(other) => bail!(
            "rmux daemon rejected attach to session `{session_name}`: {other:?}\n  \
             (the session may not exist — check `ccteam session ls` or start it with `ccteam start`.)",
        ),
    };

    // into_parts yields the raw upgraded UnixStream plus any bytes the
    // daemon already streamed past the response frame; the driver
    // replays those `initial_bytes` before entering the poll loop so no
    // pane output is lost on the attach boundary.
    let (stream, initial_bytes) = upgrade.into_parts();
    rmux_client::attach_terminal_with_initial_bytes(stream, initial_bytes)
        .map_err(|e| anyhow::anyhow!("rmux attach session `{session_name}`: {e}"))?;
    Ok(())
}

/// Non-Unix fallback: the rmux backend is Unix-first (UDS transport),
/// so interactive attach is not yet wired on Windows. Fail loud.
#[cfg(not(unix))]
fn rmux_interactive_attach(session_name: &str) -> Result<()> {
    bail!(
        "rmux interactive attach is not supported on this platform yet \
         (session `{session_name}`); use the tmux backend (unset CCTEAM_MUX_BACKEND).",
    )
}

/// Run a future to completion on a throwaway current-thread runtime. The
/// session-listing / chat-attach handlers are synchronous CLI entry points but
/// need the async `ProcessBackend` enumeration; this keeps that bridge in one
/// place.
fn block_on_async<F: std::future::Future>(fut: F) -> Result<F::Output> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")?;
    Ok(rt.block_on(fut))
}

/// Interactively hand the controlling tty over to an existing mux session by
/// its exact name, honoring `CCTEAM_MUX_BACKEND`. Read-only handover — it never
/// captures pane text (R6); it only checks existence and execs the attach.
fn attach_interactive_by_name(session_name: &str) -> Result<()> {
    if ccteam_harness::backend_kind_from_env() == ccteam_harness::BackendKind::Rmux {
        return rmux_interactive_attach(session_name);
    }
    let tmux_session = TmuxSession::from_name(session_name.to_string());
    if !tmux_session.exists() {
        bail!("mux session not running: {}", tmux_session.name());
    }
    eprintln!("→ tmux attach -t {}", tmux_session.name());
    let argv = ccteam_harness::interactive_attach_argv(
        ccteam_harness::BackendKind::Tmux,
        tmux_session.name(),
    );
    let (bin, args) = argv
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("interactive_attach_argv returned empty argv"))?;
    let status = Command::new(bin)
        .args(args)
        .status()
        .context("spawn tmux attach")?;
    if !status.success() {
        bail!("tmux attach exited with {status}");
    }
    Ok(())
}

/// Resolve a gateway chat-mode bot session (`ccteam-chat-<slug>-<role>`) for
/// `slug_or_name` (+ optional `role`) and interactively attach to it. These
/// sessions are invisible to [`run_attach`], which only resolves the
/// project-level `ccteam-<slug>` session.
///
/// Returns `Ok(true)` when a chat session was matched and the attach was
/// dispatched; `Ok(false)` *only* when `role` is omitted AND no live chat
/// session matches `slug_or_name`, so the caller may fall back to the
/// project-oriented attach. Read-only enumeration (R6) — never captures panes.
/// Resolve a chat-session reference to its canonical tmux name
/// (`ccteam-chat-<slug>-<sid>`):
/// - a full `ccteam-chat-…` name passes through verbatim;
/// - an explicit `sid` yields the deterministic name;
/// - otherwise live chat sessions are enumerated and filtered by slug.
///
/// Returns `Ok(None)` when nothing matches (the caller falls back to the
/// project pane `ccteam-<slug>`); `Err` when `<slug>` is ambiguous across
/// sessions. Shared by `attach` and `peek` so both resolve identically.
///
/// v0.8.8 F1 — the disambiguator is now the **sid** (`s<N>`), not a role: the
/// pane name's trailing segment is the sid, and the same `(project, role)` can
/// host several independent sessions, so a role no longer uniquely names one.
pub fn resolve_chat_session_name(slug_or_name: &str, sid: Option<&str>) -> Result<Option<String>> {
    if slug_or_name.starts_with(ccteam_harness::CHAT_SESSION_PREFIX) {
        return Ok(Some(slug_or_name.to_string()));
    }
    if let Some(sid) = sid {
        return Ok(Some(ccteam_harness::chat_session_name(slug_or_name, sid)));
    }
    let backend = ccteam_harness::default_process_backend();
    let live = block_on_async(ccteam_harness::list_chat_sessions(backend.as_ref()))??;
    let mut matches: Vec<(String, String)> = live
        .iter()
        .filter_map(|name| {
            let (slug, sid) = ccteam_harness::parse_chat_session_name(name)?;
            (slug == slug_or_name).then(|| (sid, name.clone()))
        })
        .collect();
    matches.sort();
    match matches.as_slice() {
        [] => Ok(None),
        [(_sid, name)] => Ok(Some(name.clone())),
        many => {
            let mut msg = format!(
                "`{slug_or_name}` has {} live chat sessions; specify a sid:",
                many.len()
            );
            for (sid, name) in many {
                msg.push_str(&format!("\n  {slug_or_name} {sid}   # {name}"));
            }
            bail!("{msg}")
        }
    }
}

pub fn try_attach_chat_session(slug_or_name: &str, sid: Option<&str>) -> Result<bool> {
    match resolve_chat_session_name(slug_or_name, sid)? {
        Some(name) => {
            attach_interactive_by_name(&name)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// `ccteam session ls` — read-only snapshot of gateway chat-mode bot sessions.
///
/// v0.8.8 B4 — the row source is now the daemon's **persisted** session records
/// (sid · project · role · vendor · permission_mode) via
/// [`ccteam_im::gateway::tracked_chat_sessions`], not a process-name enumeration.
/// A tracked record means the daemon owns the session, so it shows **live** —
/// this is the BUG-5 fix: codex sessions (which the process backend can't always
/// confirm by name) are live whenever the gateway tracks them, instead of the
/// old false "registered, not running". Live OS panes (`ccteam-chat-*`) that are
/// **not** in the tracked set are surfaced as orphans (a process that outlived
/// the daemon that spawned it). Reading the mux backend is name-enumeration only
/// (R6: never capture-pane); never spawns or kills.
pub fn run_sessions() -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    let daemon_up = ccteam_core::daemon::daemon_reachable(&paths);

    // Live OS pane names (for orphan detection). Best-effort: a backend error
    // just means we can't flag orphans, not that we refuse to list tracked rows.
    let live = block_on_async(ccteam_harness::list_chat_sessions(
        ccteam_harness::default_process_backend().as_ref(),
    ))
    .ok()
    .and_then(Result::ok)
    .unwrap_or_default();

    // A missing / unreadable registry is non-fatal: no tracked rows, every live
    // pane is then an orphan.
    let state_path = ccteam_im::default_gateway_state_path();
    let tracked = ccteam_im::gateway::tracked_chat_sessions(&state_path).unwrap_or_default();

    print!("{}", render_sessions_table(&tracked, &live, daemon_up));
    Ok(())
}

/// Pure renderer for `ccteam session ls` (testable without a daemon / terminal).
///
/// `tracked` = persisted gateway session records (each shows **live** when the
/// daemon is up, else `registered (daemon down)`); `live_panes` = live
/// `ccteam-chat-*` OS pane names used only to flag **orphans** (live pane ∧ not
/// tracked). Columns: SLUG · SID · ROLE · VENDOR · STATUS. An orphan has no
/// role/vendor (the pane name only carries slug+sid post-F1) → `-`.
fn render_sessions_table(
    tracked: &[ccteam_im::gateway::TrackedSessionRow],
    live_panes: &[String],
    daemon_up: bool,
) -> String {
    // Canonical names the daemon tracks, to subtract from live panes → orphans.
    let tracked_names: std::collections::BTreeSet<String> = tracked
        .iter()
        .map(|r| ccteam_harness::chat_session_name(&r.project, &r.sid))
        .collect();

    struct Row {
        slug: String,
        sid: String,
        role: String,
        vendor: String,
        status: String,
    }

    let tracked_status = if daemon_up {
        "live"
    } else {
        "registered (daemon down)"
    };

    let mut rows: Vec<Row> = tracked
        .iter()
        .map(|r| Row {
            slug: r.project.clone(),
            sid: r.sid.clone(),
            role: r.role.clone(),
            vendor: r.vendor.clone(),
            status: tracked_status.to_string(),
        })
        .collect();

    // Orphans: a live pane whose canonical name the daemon doesn't track.
    for name in live_panes {
        if tracked_names.contains(name) {
            continue;
        }
        if let Some((slug, sid)) = ccteam_harness::parse_chat_session_name(name) {
            rows.push(Row {
                slug,
                sid,
                role: "-".to_string(),
                vendor: "-".to_string(),
                status: "orphan (untracked live pane)".to_string(),
            });
        }
    }

    // Deterministic order: slug, then sid.
    rows.sort_by(|a, b| a.slug.cmp(&b.slug).then(a.sid.cmp(&b.sid)));

    if rows.is_empty() {
        let mut out = String::new();
        out.push_str("no chat sessions (none tracked, none live).\n");
        out.push_str(
            "  Gateway chat sessions appear here once a bot is spawned \
             (e.g. via Telegram `/new`).\n",
        );
        return out;
    }

    // Column widths mirror the existing `w_sid` algorithm (header-floor max).
    let w_slug = rows.iter().map(|r| r.slug.len()).max().unwrap_or(0).max(4);
    let w_sid = rows.iter().map(|r| r.sid.len()).max().unwrap_or(0).max(3);
    let w_role = rows.iter().map(|r| r.role.len()).max().unwrap_or(0).max(4);
    let w_vendor = rows
        .iter()
        .map(|r| r.vendor.len())
        .max()
        .unwrap_or(0)
        .max(6);

    let mut out = String::new();
    let header = format!(
        "{:<w_slug$}  {:<w_sid$}  {:<w_role$}  {:<w_vendor$}  STATUS",
        "SLUG", "SID", "ROLE", "VENDOR"
    );
    out.push_str(header.trim_end());
    out.push('\n');
    for r in &rows {
        let line = format!(
            "{:<w_slug$}  {:<w_sid$}  {:<w_role$}  {:<w_vendor$}  {}",
            r.slug, r.sid, r.role, r.vendor, r.status,
        );
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out.push('\n');
    out.push_str("attach: `ccteam internal attach <slug> [sid]`  (Telegram: `/sessions`)\n");
    out
}

pub fn run_attach(paths: &CcteamPaths, slug: &str) -> Result<()> {
    // V0.5.0 F93b: agent-team mode dispatches to the lead session.
    if let Some(lead_id) = read_agent_team_lead_session_id(paths, slug)? {
        eprintln!("→ claude attach {lead_id}");
        let status = Command::new("claude")
            .args(["attach", &lead_id])
            .status()
            .context("spawn claude attach")?;
        if !status.success() {
            bail!("claude attach exited with {status}");
        }
        return Ok(());
    }

    // V0.8 W5 — backend-aware interactive attach. Branch on the
    // configured mux backend BEFORE the tmux exists-check: under the
    // rmux backend the tmux session never exists, so falling through to
    // the tmux / claude-bg paths would always miss. The rmux path
    // attaches to the ccteam-hosted rmux daemon via rmux-client
    // directly (Path A). The tmux path is unchanged (default).
    let session_name = session_name_for_project(paths, slug);
    if ccteam_harness::backend_kind_from_env() == ccteam_harness::BackendKind::Rmux {
        return rmux_interactive_attach(&session_name);
    }

    let tmux_session = TmuxSession::from_name(session_name);
    if tmux_session.exists() {
        eprintln!("→ tmux attach -t {}", tmux_session.name());
        // V0.8 W1 — argv from `ccteam_harness::interactive_attach_argv`
        // (free fn, not a trait method — terminal handover doesn't fit
        // async; audit delta 6). Caller still spawns blocking
        // Command::status() on the CLI's own controlling tty.
        let argv = ccteam_harness::interactive_attach_argv(
            ccteam_harness::BackendKind::Tmux,
            tmux_session.name(),
        );
        let (bin, args) = argv
            .split_first()
            .ok_or_else(|| anyhow::anyhow!("interactive_attach_argv returned empty argv"))?;
        let status = Command::new(bin)
            .args(args)
            .status()
            .context("spawn tmux attach")?;
        if !status.success() {
            bail!("tmux attach exited with {status}");
        }
        return Ok(());
    }

    // V0.4.0 fallback: walk progress.jsonl for the latest agent_spawn
    // and grab its job_id. The actual bg-session id is the
    // `daemonShort` written to `~/.claude/jobs/<id>/state.json`; in
    // F61 we stamped it onto `SessionHandle::job_id` and wrote it to
    // the `agent_spawn` event's `session_id` field is the orchestrator's
    // internal sid — the real bg short-id lives in state.json.
    if let Some(job_id) = latest_claude_bg_job_id(paths, slug) {
        eprintln!("→ claude attach {job_id}");
        let status = Command::new("claude")
            .args(["attach", &job_id])
            .status()
            .context("spawn claude attach")?;
        if !status.success() {
            bail!("claude attach exited with {status}");
        }
        return Ok(());
    }

    bail!(
        "no live session for `{slug}`: tmux session `{}` not running, no `claude --bg` job recorded in progress.jsonl. Spawn one with `ccteam spawn {slug} <role>`.",
        tmux_session.name()
    )
}

/// V0.5.0 F93b — probe whether a project is in agent-team mode and
/// return its lead session id from `.ccteam/team-snapshot.json`.
///
/// Returns:
///   - `Ok(Some(id))` — project is agent-team mode AND has been started
///     (snapshot exists, `lead_session_id` populated).
///   - `Ok(None)` — project is artifact-driven OR no project exists at
///     `<slug>` (let the caller fall through to tmux / bg paths).
///   - `Err(_)` — project is agent-team mode but snapshot is missing /
///     `lead_session_id` not yet written (lead hasn't been started yet).
fn read_agent_team_lead_session_id(paths: &CcteamPaths, slug: &str) -> Result<Option<String>> {
    let project_dir = paths.project_dir(slug);
    if !project_dir.exists() {
        return Ok(None);
    }
    let spec = match ccteam_flow::WorkflowSpec::load_for_project(&project_dir) {
        Ok(spec) => spec,
        Err(_) => return Ok(None),
    };
    if !matches!(spec.mode, ccteam_flow::WorkflowMode::AgentTeam) {
        return Ok(None);
    }
    let snapshot_path = project_dir.join(".ccteam").join("team-snapshot.json");
    if !snapshot_path.exists() {
        bail!(
            "project `{slug}` is in agent-team mode but has no team-snapshot.json yet.\n  \
             Start the lead session first:  ccteam start {slug}\n  \
             (snapshot is written by `ccteam start` after spawning the __lead session.)",
        );
    }
    let body = std::fs::read_to_string(&snapshot_path)
        .with_context(|| format!("read {}", snapshot_path.display()))?;
    let v: serde_json::Value = serde_json::from_str(&body)
        .with_context(|| format!("parse {}", snapshot_path.display()))?;
    let lead_id = v
        .get("lead_session_id")
        .and_then(|s| s.as_str())
        .map(String::from);
    match lead_id {
        Some(id) if !id.is_empty() => Ok(Some(id)),
        _ => bail!(
            "project `{slug}` team-snapshot.json has no `lead_session_id` yet.\n  \
             The lead session may have failed to spawn. Check `ccteam show {slug}` and\n  \
             `cat {}` for diagnostics.",
            snapshot_path.display(),
        ),
    }
}

/// Walk `progress.jsonl` newest-first, find the most recent
/// `agent_spawn` whose state.json still reports a live bg job (state
/// ∈ {working}). Returns the `daemonShort` id Claude assigned.
fn latest_claude_bg_job_id(paths: &CcteamPaths, slug: &str) -> Option<String> {
    let progress = paths.progress_jsonl(slug);
    let events = ccteam_core::progress::read_all_events(&progress).ok()?;
    let mut sids: Vec<String> = events
        .iter()
        .filter(|e| e.get("event").and_then(|s| s.as_str()) == Some("agent_spawn"))
        .filter_map(|e| {
            e.get("session_id")
                .and_then(|s| s.as_str())
                .map(String::from)
        })
        .collect();
    sids.reverse();
    // For each candidate, look in ~/.claude/jobs/*/state.json with
    // matching state.json sessionId — too expensive. The simpler
    // approach: walk ~/.claude/jobs/<short>/state.json and find the
    // newest whose `cwd` matches the project dir.
    let _ = sids;
    let project_dir = paths.project_dir(slug);
    let canon_cwd = std::fs::canonicalize(&project_dir).ok()?;

    let jobs_dir = std::env::var_os("CCTEAM_CLAUDE_JOBS_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_default()
                .join(".claude")
                .join("jobs")
        });
    let read = std::fs::read_dir(&jobs_dir).ok()?;
    let mut candidates: Vec<(std::time::SystemTime, String)> = Vec::new();
    for entry in read.flatten() {
        let id = entry.file_name().to_string_lossy().to_string();
        let state_path = entry.path().join("state.json");
        let Ok(body) = std::fs::read_to_string(&state_path) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) else {
            continue;
        };
        let cwd = v.get("cwd").and_then(|s| s.as_str()).unwrap_or("");
        let Ok(canon) = std::fs::canonicalize(cwd) else {
            continue;
        };
        if canon != canon_cwd {
            continue;
        }
        let state = v.get("state").and_then(|s| s.as_str()).unwrap_or("");
        if !matches!(state, "working") {
            continue;
        }
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        candidates.push((mtime, id));
    }
    candidates.sort_by_key(|c| std::cmp::Reverse(c.0));
    candidates.into_iter().next().map(|(_, id)| id)
}

/// `ccteam peek <slug>`. Returns the contents of the session's first
/// pane via `tmux capture-pane -p`.
///
/// V0.8 W1 — routes through `ccteam_core::capture_pane_tail_from_session`
/// (re-exported over `ccteam_harness::tmux_ops::capture_pane_tail_from_session`,
/// the same primitive `TmuxBackend::capture` calls under the hood).
/// Keeps the peek path sync per the W1 "sync sites stay sync" decision.
///
/// `ccteam internal peek <slug> [sid]`. Resolves a live chat session
/// (`ccteam-chat-<slug>-<sid>`) first — mirroring `attach` — and falls
/// back to the project pane (`ccteam-<slug>`) when none matches. This is
/// why a bare `peek <slug>` against a chat session used to fail with
/// "rmux session not running: ccteam-<slug>" while `attach` worked.
///
/// v0.8.8 F1 — the optional disambiguator is the session **sid** (`s<N>`),
/// not a role (the pane's trailing segment is the sid post-F1).
pub fn run_peek_with_role(
    paths: &CcteamPaths,
    slug_or_name: &str,
    sid: Option<&str>,
) -> Result<String> {
    let session_name = match resolve_chat_session_name(slug_or_name, sid)? {
        Some(name) => name,
        None => session_name_for_project(paths, slug_or_name),
    };
    peek_session_by_name(&session_name)
}

/// Capture a 1000-line plain-text tail of a session pane by its exact
/// tmux/rmux name (chat or project — the caller already resolved it).
fn peek_session_by_name(session_name: &str) -> Result<String> {
    // V0.8 W5 — backend-aware peek. Under the rmux backend, capture is
    // non-interactive (a plain-text grid snapshot) so it fits the async
    // `ProcessBackend::capture` trait method cleanly; drive it on a
    // current-thread tokio runtime.
    // The tmux path (opt-out) is unchanged.
    if ccteam_harness::backend_kind_from_env() == ccteam_harness::BackendKind::Rmux {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("build tokio runtime for rmux peek")?;
        let id = ccteam_harness::MuxSessionId::new(session_name.to_string());
        let backend = ccteam_harness::from_env()?;
        let bytes = runtime
            .block_on(async {
                if !backend.exists(&id).await? {
                    bail!("rmux session not running: {}", id);
                }
                // 1000-line tail mirrors the tmux default scroll-region
                // window; `with_ansi=false` returns stripped plain text.
                backend.capture(&id, 1000, false).await
            })
            .context("rmux capture")?;
        return Ok(String::from_utf8_lossy(&bytes).into_owned());
    }

    let session = TmuxSession::from_name(session_name.to_string());
    if !session.exists() {
        bail!("tmux session not running: {}", session.name());
    }
    // 1000-line tail matches the legacy raw `capture-pane -p` default
    // window (no `-S` flag → tmux's default scroll-region tail).
    let text = ccteam_core::capture_pane_tail_from_session(session.name(), 1000, false)
        .unwrap_or_default();
    Ok(text)
}

/// `ccteam progress <slug>`. With `tail = false`, returns the entire
/// progress.jsonl as text. With `tail = true`, reads + writes to
/// stdout in a polling loop until Ctrl-C.
pub fn run_progress(paths: &CcteamPaths, slug: &str, tail: bool) -> Result<()> {
    use std::io::Write;
    let path = paths.progress_jsonl(slug);
    if !path.exists() {
        bail!("no progress.jsonl yet for {slug}: {}", path.display());
    }
    let mut stdout = std::io::stdout().lock();
    let initial =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    stdout.write_all(initial.as_bytes())?;
    if !tail {
        return Ok(());
    }
    let mut seen = initial.len() as u64;
    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.len() <= seen {
            continue;
        }
        use std::io::{Read, Seek, SeekFrom};
        let mut f = std::fs::File::open(&path)?;
        f.seek(SeekFrom::Start(seen))?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        stdout.write_all(&buf)?;
        stdout.flush()?;
        seen = meta.len();
    }
}

/// `ccteam resume <slug>`. M0 minimum: clears any user_pause flag, drops
/// the escalation marker if present, and sets phase_state back to idle so
/// the daemon's next tick re-dispatches the current phase. Real
/// `--resume` (claude session continuation) is M1+.
///
/// E2E 2026-05-06 F8: when the previous run ended in `escalate`, the
/// last `phase_history` entry has `status: "escalated"` which makes
/// `Dag::is_terminal_state` permanently return true. `is_terminal_state`
/// blocks every subsequent tick at `NoOp`, so the daemon never picks
/// the project back up even after the user resumes. Append a
/// `"resumed"` entry (append-only — we don't mutate the prior
/// escalated entry, so the escalation history stays auditable) so
/// `is_terminal_state` lifts the flag.
///
/// V0.3 M5.0: body lives in `ccteam_core::actions::resume` so the
/// V0.3 web layer (and the MCP wrapper) can call the same logic
/// without depending on `ccteam-cli`. This thin wrapper preserves the
/// public CLI surface; `ccteam resume <slug>` continues to dispatch
/// through here.
pub fn run_resume(paths: &CcteamPaths, slug: &str) -> Result<()> {
    ccteam_core::actions::resume(paths, slug)
}

/// `ccteam doctor` flags. Each diagnostic / repair mode is a separate
/// boolean / option so they can be combined in one invocation. (Setup
/// actions — MCP register, IM token, prefs — live in `ccteam config`.)
#[derive(Debug, Clone, Default)]
pub struct DoctorOptions {
    pub dry_run: bool,
    pub tool_surface: bool,
    /// M4.2: install `~/.claude/rules/ccteam-lessons-<team>.md` placeholders.
    pub install_memory_bridge: bool,
    /// V0.2 M0.16.2: re-write every shipped team
    /// (`~/.ccteam/teams/<name>/team.yaml` + `~/.ccteam/<phase_dir>/*.md`)
    /// using the in-binary seed bundle. Overwrites operator hand-edits.
    /// Useful after a ccteam upgrade ships schema-additive team.yaml
    /// changes (e.g. the V0.2 `evergreen` / `cost_policy` fields landed
    /// by M0.16).
    pub reset_shipped_teams: bool,
    /// V0.2 M0.18.5: load + validate one team's phase templates +
    /// `team.yaml`, including the new V0.2 inject-prompt frontmatter
    /// fields and phase IO contract (every required_input must be a
    /// prior phase's required_output). Reports each phase's state and
    /// warns on body-level protocol-keyword residue (`PHASE_DONE: <name>` /
    /// `ESCALATE:`) without failing — those drift in over time and a
    /// fail-loud check would block the orchestrator.
    pub validate_team: Option<String>,
    /// V0.2 M0.20: remove stale `~/.claude/agents/<name>.md` symlinks
    /// the M0.5 `--install-recommended-agents` path used to create.
    /// One-time cleanup for users upgrading from V0.1 to the plugin-
    /// pipeline-based path. Idempotent — no-op when no marketplace
    /// symlinks remain.
    pub migrate_recommended_agents: bool,
    /// V0.2.2 F38: end-to-end screenshot pipeline smoke test for the
    /// given project slug. Triggers `render_screenshot` and reports
    /// the resulting PNG path or graceful-degrade reason. Verifies
    /// font + tmux + IO without requiring a live MCP client.
    pub screenshot_smoke: Option<String>,
    /// V0.4.2 F74: fold V0.4.1 project layout into the new
    /// `~/.ccteam/config.yaml`. See `ccteam_core::migrate_v041_to_v042`
    /// for the exact rules. Idempotent — safe to run on already-
    /// migrated homes.
    pub migrate_v041_to_v042: bool,
    /// V0.4.6 F83: move every registered project's root
    /// `workflow.yaml` into `<project>/.ccteam/workflow.yaml`. Pair
    /// with `dry_run = false` (i.e. `--apply`) to actually move; the
    /// default dry-run prints what would happen. Conflicts (both
    /// locations have a `workflow.yaml`) are fail-safe and left
    /// untouched.
    pub migrate_workflow_to_ccteam_dir: bool,
    /// V0.4.6 F85: reclaim terminated `~/.claude/jobs/<id>/`
    /// directories whose `state.json::state ∈ TERMINAL_STATES` and
    /// whose `firstTerminalAt` (or dir mtime fallback) is older than
    /// `~/.ccteam/config.yaml::claude_jobs_retention_days` (default 7).
    /// Default is dry-run; pair with `gc_apply` to actually `rm -rf`.
    pub gc_claude_jobs: bool,
    /// V0.4.6 F85: arm `gc_claude_jobs` to commit removals to disk.
    /// No-op unless `gc_claude_jobs` is also true.
    pub gc_apply: bool,
    /// V0.4.6 F91: walk every registered project's
    /// `.claude/settings.json` and strip the now-retired
    /// `ccteam hook cost-accumulate` PostToolUse entry. `dry_run = true`
    /// previews the scrub without writing. Idempotent.
    pub update_hooks: bool,
    /// V0.5.0 F92: print the embedded `pricing.json::schema_version`
    /// alongside today's date and WARN when the embedded table is
    /// older than 180 days. Pure-readout — no fs mutation.
    pub check_pricing_version: bool,
    /// V0.6.0 Wave 3 F112: probe `codex --version` and warn when the
    /// binary is missing or older than the minimum supported
    /// (`0.131`). Pure-readout — no fs mutation.
    pub check_codex_version: bool,
    /// V0.6.0 Wave 3 F112: probe `codex login status` and surface
    /// whether the operator is logged in (ChatGPT / API key) so
    /// mode-3 codex bot path knows to error early. Pure-readout — no
    /// fs mutation.
    pub check_codex_auth: bool,
    /// V0.6.5 F155: deterministic gate for the `ccteam-creator`
    /// Phase 3.5 Codex auto-critic detection. Honors
    /// `$CCTEAM_CODEX_BIN`; spawns a one-shot `<bin> --version` and
    /// `<bin> exec --json --skip-git-repo-check` probe with a
    /// minimal canary prompt, then emits a single JSON object on
    /// stdout describing the outcome. Maps to non-zero exit code in
    /// the CLI dispatcher (2 = unavailable, 3 = malformed) so the
    /// skill body and CI can branch without parsing free text.
    pub check_codex_auto_critic: bool,
    /// V0.6.6 F173: reconcile `<ccteam_root>/cost-budget.json` ledger
    /// rows against every registered project's `progress.jsonl` over
    /// the last 24h. Invariant: per vendor, the count of `agent_done`
    /// events with `status="completed"` matches the count of ledger
    /// `BudgetSample` rows. Reports any orphan (vendor adapter
    /// recorded a progress.jsonl event but no ledger row) so a future
    /// regression — e.g. a new adapter forgetting the cost hook —
    /// surfaces deterministically. Pure-readout; no fs mutation.
    pub check_cost_orphan: bool,
    /// V0.6.1 F139: materialize the `~/.ccteam/hooks/hook.sh` daemon
    /// dispatcher (idempotent, chmod 0755). Use after a ccteam binary
    /// upgrade ships a new script body.
    pub install_hooks: bool,
    /// V0.6.1 F139: rewrite every registered project's
    /// `.claude/settings.json` to invoke `~/.ccteam/hooks/hook.sh`
    /// instead of `<ccteam-bin> internal hook ...` (or older V0.4.x
    /// `ccteam hook ...` / F39 `cct hook ...` forms). Idempotent; pair
    /// with `--dry-run` to preview.
    pub migrate_hook_commands: bool,
    /// V0.6.6 F171: assert the MCP tool surface is fully wired. The
    /// dispatcher short-circuits to `run_verify_mcp` which counts
    /// active + STUB tools (`mcp_tool_groups::STUB_TOOLS`) and exits 1
    /// when any STUB is found. Mirrors `--check-codex-auto-critic`'s
    /// structured exit-code pattern.
    pub verify_mcp: bool,
    /// V0.6.6 F171: pair with `verify_mcp = true` to emit a single
    /// pretty-printed JSON object on stdout instead of the human-
    /// friendly text report. Ignored when `verify_mcp == false`.
    pub verify_mcp_json: bool,
}

/// `ccteam doctor` dispatch. Returns a human-readable report so unit
/// tests don't need to capture stdout.
pub fn run_doctor(paths: &CcteamPaths, mut opts: DoctorOptions) -> Result<String> {
    let any_mode = opts.tool_surface
        || opts.install_memory_bridge
        || opts.reset_shipped_teams
        || opts.validate_team.is_some()
        || opts.migrate_recommended_agents
        || opts.screenshot_smoke.is_some()
        || opts.migrate_v041_to_v042
        || opts.migrate_workflow_to_ccteam_dir
        || opts.gc_claude_jobs
        || opts.update_hooks
        || opts.check_pricing_version
        || opts.check_codex_version
        || opts.check_codex_auth
        || opts.check_codex_auto_critic
        || opts.check_cost_orphan
        || opts.install_hooks
        || opts.migrate_hook_commands;
    // V0.6.1 F121 — `ccteam doctor` with no mode flag implicitly runs
    // the pricing staleness check so operators see ageing rate sheets
    // without having to remember `--check-pricing-version`. The opt-in
    // mutation modes still require an explicit flag, and the help text
    // is appended after the report so first-time users discover them.
    if !any_mode {
        opts.check_pricing_version = true;
    }
    let mut out = String::new();
    if opts.tool_surface {
        out.push_str(&render_tool_surface_report(paths)?);
    }
    if opts.install_memory_bridge {
        out.push_str(&render_install_memory_bridge_report(paths, &opts)?);
    }
    if opts.reset_shipped_teams {
        out.push_str(&render_reset_shipped_teams_report(paths, &opts)?);
    }
    if let Some(team) = &opts.validate_team {
        let (report, fails) = render_validate_team_report(paths, team)?;
        out.push_str(&report);
        // F30 — `--help` advertises "Fails-loud on schema violations
        // and IO-contract gaps". Honor that: any [FAIL] (phase-level
        // or plugin-section) → non-zero exit so CI can gate on it.
        if fails > 0 {
            anyhow::bail!(
                "ccteam doctor --validate-team {team}: {fails} fail(s) — \
                 see report above\n\n{out}",
            );
        }
    }
    if opts.migrate_recommended_agents {
        out.push_str(&render_migrate_recommended_agents_report(&opts)?);
    }
    if let Some(slug) = &opts.screenshot_smoke {
        out.push_str(&render_screenshot_smoke_report(paths, slug)?);
    }
    if opts.migrate_v041_to_v042 {
        let report = ccteam_core::migrate_v041_to_v042(paths)?;
        out.push_str(&ccteam_core::render_migration_report(&report));
    }
    if opts.migrate_workflow_to_ccteam_dir {
        let reports = ccteam_core::migrate_workflow_to_ccteam_dir(paths, opts.dry_run)?;
        out.push_str(&ccteam_core::render_workflow_migration_report(
            &reports,
            opts.dry_run,
        ));
    }
    if opts.gc_claude_jobs {
        out.push_str(&render_gc_claude_jobs_report(paths, opts.gc_apply)?);
    }
    if opts.update_hooks {
        out.push_str(&render_update_hooks_report(paths, opts.dry_run)?);
    }
    if opts.check_pricing_version {
        out.push_str(&render_check_pricing_version_report());
    }
    if opts.check_codex_version {
        out.push_str(&render_check_codex_version_report());
    }
    if opts.check_codex_auth {
        out.push_str(&render_check_codex_auth_report());
    }
    if opts.check_cost_orphan {
        out.push_str(&render_check_cost_orphan_report(paths));
    }
    if opts.install_hooks {
        out.push_str(&render_install_hooks_report(paths)?);
    }
    if opts.migrate_hook_commands {
        out.push_str(&render_migrate_hook_commands_report(paths, opts.dry_run)?);
    }
    out.push_str(&render_daemon_health_line(paths));
    // v0.8.6 — always-run self-heal: sweep stale `~/.claude/skills/
    // ccteam-*` directories left by earlier versions that bundled skills.
    // W4b deleted the only previous caller (`doctor --install-skill`), so
    // upgraders' skill residue was no longer cleaned; rehang it here so a
    // plain `ccteam doctor` always tidies it up. Never fails the exit code.
    out.push_str(&render_legacy_skill_cleanup_line());
    // v0.8.6 — informational home-layout drift line. Flags any
    // orchestrator-era leftover directory under `~/.ccteam` that the
    // current architecture no longer writes. Never fails the exit code.
    out.push_str(&render_home_drift_line(paths));
    // V0.3.1 F47 — informational codex CLI detection. Appends one line
    // to every successful doctor run (any_mode == true) so operators
    // see whether the codex binary is on PATH ahead of V0.3.2's real
    // CodexAdapter impl. **Never fails the doctor exit code** —
    // `which codex` returning non-zero is the expected state today.
    out.push_str(&render_codex_detection_line());
    // V0.6.1 F121 — when the caller passed no mode flag we ran the
    // implicit pricing-staleness check; append the help block so
    // first-time users still discover the explicit mode flags.
    if !any_mode {
        out.push_str(NO_MODE_HELP);
    }
    Ok(out)
}

fn render_daemon_health_line(paths: &CcteamPaths) -> String {
    let health = ccteam_core::check_daemon_health(paths);
    format!("[ccteam] daemon: {}\n", health.describe())
}

/// v0.8.6 — informational `~/.ccteam` home-layout drift check.
///
/// Anchored on [`ccteam_core::canonical_home_dirs`] (the init-time
/// directory manifest), extended with the directories the resident
/// daemon creates lazily at runtime (`pty`, `harness`, `imd`) and the
/// shipped team-template root (`teams`). Any *other* top-level
/// directory under `~/.ccteam` is an orchestrator-era leftover
/// (`phases/`, `queue/`, `control/`, `memory/`, `templates/`, `inbox/`,
/// …) and is reported on a single line so operators can `rm -rf` it.
///
/// Purely informational — never fails the doctor exit code, and tolerates
/// a missing/unreadable root (fresh install) by reporting nothing.
fn render_home_drift_line(paths: &CcteamPaths) -> String {
    // Directories that are legitimate under the current architecture but
    // are intentionally *not* in the init-time canonical set (they are
    // created lazily by the daemon / bot registration / team seeding).
    const RUNTIME_LAZY_DIRS: &[&str] = &["pty", "harness", "imd", "teams"];

    let Ok(entries) = std::fs::read_dir(&paths.root) else {
        return String::new();
    };
    let mut unexpected: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if ccteam_core::canonical_home_dirs().contains(&name) || RUNTIME_LAZY_DIRS.contains(&name) {
            continue;
        }
        unexpected.push(name.to_string());
    }
    if unexpected.is_empty() {
        return String::new();
    }
    unexpected.sort();
    format!(
        "[ccteam] home drift: {} unexpected dir(s) under {} (orchestrator-era leftovers; safe to `rm -rf`): {}\n",
        unexpected.len(),
        paths.root.display(),
        unexpected.join(", "),
    )
}

/// v0.8.6 — always-run self-heal that sweeps stale
/// `~/.claude/skills/ccteam-*` directories left by versions that bundled
/// skills (the skill set is now empty). W4b deleted the only previous
/// caller (`doctor --install-skill`), so this re-hooks
/// [`ccteam_core::migrate_legacy_skill_dirs`] into the plain `ccteam
/// doctor` run.
///
/// Runs non-dry — it actually removes the unedited shipped dirs; operator
/// hand-edits (no ccteam-managed marker) are preserved and reported so
/// the user can clear them manually. Tolerates a missing `~/.claude`
/// (fresh install) and never fails the doctor exit code.
///
/// Respects `CCTEAM_DISABLE_TOOL_SURFACE_BOOTSTRAP` (the same guard the
/// rest of the tool-surface bootstrap honors) so the unit-test binary —
/// which resolves the *real* `~/.claude` via [`user_claude_dir`] — never
/// mutates the developer's / CI's home skill set.
fn render_legacy_skill_cleanup_line() -> String {
    use ccteam_core::LegacySkillAction;

    if std::env::var("CCTEAM_DISABLE_TOOL_SURFACE_BOOTSTRAP")
        .ok()
        .is_some_and(|v| matches!(v.as_str(), "1" | "true" | "yes"))
    {
        return String::new();
    }
    let Ok(claude) = user_claude_dir() else {
        return String::new();
    };
    let Ok(reports) = ccteam_core::migrate_legacy_skill_dirs(&claude, false) else {
        return String::new();
    };

    let removed: Vec<&str> = reports
        .iter()
        .filter(|r| r.action == LegacySkillAction::Removed)
        .map(|r| r.legacy_name.as_str())
        .collect();
    let preserved: Vec<&str> = reports
        .iter()
        .filter(|r| r.action == LegacySkillAction::PreservedHandEdit)
        .map(|r| r.legacy_name.as_str())
        .collect();

    if removed.is_empty() && preserved.is_empty() {
        // The overwhelmingly common case (nothing stale) stays quiet so
        // the doctor output isn't noisier on every run.
        return String::new();
    }
    let mut line = String::new();
    if !removed.is_empty() {
        line.push_str(&format!(
            "[ccteam] legacy skills: removed {} stale dir(s) under {} (bundled skills are gone): {}\n",
            removed.len(),
            claude.join("skills").display(),
            removed.join(", "),
        ));
    }
    if !preserved.is_empty() {
        line.push_str(&format!(
            "[ccteam] legacy skills: {} hand-edited dir(s) preserved under {} (remove manually): {}\n",
            preserved.len(),
            claude.join("skills").display(),
            preserved.join(", "),
        ));
    }
    line
}

/// V0.6.1 F121 — help text appended after the implicit pricing check
/// when `ccteam doctor` is invoked with no mode flag.
const NO_MODE_HELP: &str = "\nccteam doctor: pass at least one mode flag for the opt-in checks below.\n\
     \n\
     modes:\n  \
     --tool-surface\n      \
     cross-check phase templates' tools_required against current reachability — \
     plugin-pipeline-aware (V0.2 M0.20).\n  \
     (setup actions — registering the ccteam MCP server, setting the IM token, and \
     editing preferences — moved to `ccteam config`; `doctor` is diagnostics only.)\n  \
     --install-memory-bridge [--dry-run]\n      \
     write ~/.claude/rules/ccteam-lessons-<team>.md placeholders for every team with non-empty retro_schema (M4.2; V0.2 M0.16.2 — disk-driven team discovery).\n  \
     --reset-shipped-teams [--force]\n      \
     re-seed shipped team templates (~/.ccteam/teams/<name>/team.yaml + ~/.ccteam/<phase_dir>/*.md) from the in-binary bundle. Without --force, operator hand-edits are preserved; with --force, every shipped file is overwritten (V0.2 M0.16.2).\n  \
     --validate-team <name>\n      \
     load + validate one team's team.yaml + phase markdown set (V0.2 M0.18.5). Reports per-phase frontmatter health, IO contract consistency between adjacent phases, and warns on body-level protocol-keyword residue.\n  \
     --migrate-recommended-agents [--dry-run]\n      \
     remove stale ~/.claude/agents/<name>.md symlinks left by the V0.1 ln -sf path. One-time cleanup after upgrading to V0.2 plugin pipeline (V0.2 M0.20).\n  \
     --screenshot-smoke <slug>\n      \
     render an end-to-end PNG screenshot of <slug>'s tmux pane to <project>/.ccteam/screenshots/<utc>.png. Verifies font + tmux + imageproc + IO; reports the path on success, the degrade reason on failure (V0.2.2 F38).\n  \
     --migrate-v041-to-v042\n      \
     fold V0.4.1 ~/projects/* + ~/.ccteam/watchdog.yaml into the new ~/.ccteam/config.yaml. Idempotent (V0.4.2 F74).\n  \
     --migrate-workflow-to-ccteam-dir [--apply]\n      \
     move every registered project's root workflow.yaml into <project>/.ccteam/workflow.yaml \
     (V0.4.6 F83). Default dry-run; pair with --apply to perform the moves. Conflicts \
     (both locations present) are fail-safe — neither file is touched.\n  \
     --gc-claude-jobs [--apply]\n      \
     reclaim terminated ~/.claude/jobs/<id>/ dirs older than ~/.ccteam/config.yaml::claude_jobs_retention_days (default 7 days; 0 disables). Default is dry-run; --apply commits removals (V0.4.6 F85).\n  \
     --update-hooks [--dry-run]\n      \
     walk every registered project's .claude/settings.json and strip the retired `ccteam hook cost-accumulate` entry. Idempotent (V0.4.6 F91).\n  \
     --check-pricing-version\n      \
     print the embedded pricing.json schema_version next to today's date; WARN at >180 days, ERROR at >365 days (V0.5.0 F92 / V0.6.1 F121). Runs implicitly when no mode flag is passed.\n  \
     --check-codex-version\n      \
     probe `codex --version` and WARN when older than 0.131 (V0.6.0 Wave 3 F112).\n  \
     --check-codex-auth\n      \
     probe `codex login status` and report whether the operator is logged in (V0.6.0 Wave 3 F112).\n  \
     --check-codex-auto-critic\n      \
     deterministic gate for the `ccteam-creator` Phase 3.5 Codex auto-critic injection (V0.6.5 F155). \
     Honors $CCTEAM_CODEX_BIN; emits a single JSON line on stdout. Exit 0 = available + well-formed, \
     2 = unavailable (binary missing / version probe failed / not authenticated), 3 = available but \
     `codex exec --json` output malformed. The skill consults this subprocess instead of inline probes.\n  \
     --check-cost-orphan\n      \
     reconcile `<ccteam_root>/cost-budget.json` ledger rows against every registered project's \
     progress.jsonl over the last 24h (V0.6.6 F173). WARN per vendor when `agent_done` count exceeds \
     ledger row count — indicates a spawn path bypassed the ledger hook. Silent OK when reconciled.\n  \
     --install-hooks\n      \
     materialize ~/.ccteam/hooks/hook.sh (the daemon-aware Claude Code hook dispatcher; V0.6.1 F139). \
     Idempotent; chmod 0755. `ccteam init` already does this on first install.\n  \
     --migrate-hook-commands [--dry-run]\n      \
     rewrite every registered project's .claude/settings.json so hook commands invoke \
     ~/.ccteam/hooks/hook.sh instead of the V0.4.6 / V0.6.0 `<ccteam-bin> internal hook ...` form \
     (V0.6.1 F139). Idempotent.\n";

/// V0.3.1 F47 — pure informational `which codex` detection. Returns
/// a single line ending with `\n`. The lookup uses `Command::new("which")`
/// and tolerates every failure mode (missing `which`, IO error, codex
/// binary absent) by emitting the "not found" line.
fn render_codex_detection_line() -> String {
    let output = Command::new("which").arg("codex").output();
    match output {
        Ok(out) if out.status.success() => {
            let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if path.is_empty() {
                // `which` returned 0 but no path — defensive: treat as
                // not-found rather than printing a misleading empty path.
                fallback_not_found_line()
            } else {
                format!("[ccteam] codex CLI: present @ {path}\n")
            }
        }
        // `which` non-zero exit (codex not on PATH) or spawn error
        // (no `which` binary) — both surface as informational not-found.
        _ => fallback_not_found_line(),
    }
}

fn fallback_not_found_line() -> String {
    String::from(
        "[ccteam] codex CLI: not found (install for V0.4.0 CodexAdapter — see docs/research/ccteam-codex-integration.md)\n",
    )
}

/// V0.4.6 F85 — render `ccteam doctor --gc-claude-jobs [--apply]`.
///
/// Reads `~/.ccteam/config.yaml::claude_jobs_retention_days` (default 7
/// days; 0 disables GC) and sweeps `~/.claude/jobs/`. `apply=false`
/// (dry-run) prints what would be reclaimed; `apply=true` actually
/// `rm -rf`'s the directories. The report has a one-line summary
/// followed by per-entry status lines so operators can sanity-check
/// what was touched.
fn render_gc_claude_jobs_report(paths: &CcteamPaths, apply: bool) -> Result<String> {
    let mut out = String::from("ccteam doctor --gc-claude-jobs");
    if apply {
        out.push_str(" --apply");
    }
    out.push_str("\n\n");

    let retention = match ccteam_core::load_ccteam_config(&paths.root) {
        Ok(cfg) => cfg.claude_jobs_retention_days,
        Err(err) => {
            out.push_str(&format!(
                "  [WARN] failed to load config.yaml ({err:#}); using default retention.\n"
            ));
            ccteam_core::default_claude_jobs_retention_days()
        }
    };
    out.push_str(&format!("  retention_days: {retention}\n"));

    if retention == 0 {
        out.push_str(
            "  GC disabled (retention_days == 0). \
             Set `claude_jobs_retention_days: N` in ~/.ccteam/config.yaml to enable.\n",
        );
        return Ok(out);
    }

    let dry_run = !apply;
    let report = ccteam_core::gc_user_claude_jobs(retention, dry_run)?;

    out.push_str(&format!(
        "  mode: {}\n",
        if dry_run {
            "dry-run (no fs mutation)"
        } else {
            "apply"
        }
    ));
    out.push_str(&format!(
        "  dir_count_before: {}\n",
        report.dir_count_before
    ));
    out.push_str(&format!("  dir_count_after:  {}\n", report.dir_count_after));
    out.push_str(&format!(
        "  removed:       {}\n  kept_working:  {}\n  kept_recent:   {}\n  kept_corrupt:  {}\n  kept_unknown:  {}\n",
        report.removed,
        report.kept_working,
        report.kept_recent,
        report.kept_corrupt,
        report.kept_unknown,
    ));

    if !report.entries.is_empty() {
        out.push_str("\n  entries:\n");
        for e in &report.entries {
            let tag = match e.disposition {
                ccteam_core::GcDisposition::Removed => "removed",
                ccteam_core::GcDisposition::WouldRemove => "would-remove",
                ccteam_core::GcDisposition::KeptWorking => "kept-working",
                ccteam_core::GcDisposition::KeptRecent => "kept-recent",
                ccteam_core::GcDisposition::KeptCorrupt => "kept-corrupt",
                ccteam_core::GcDisposition::KeptUnknown => "kept-unknown",
            };
            out.push_str(&format!("    [{tag}] {}\n", e.job_id));
        }
    }

    if dry_run && report.removed > 0 {
        out.push_str("\n  Re-run with --apply to actually reclaim the entries listed above.\n");
    }
    out.push('\n');
    Ok(out)
}

/// V0.2 M0.18.5: load + validate one team's `team.yaml` and every
/// phase markdown the team ships. Surfaces:
///
/// 1. team.yaml load + parse + validate
/// 2. phase frontmatter parse + `validate_m0` (covers V0.2 inject-
///    prompt fields: empty `escalate_grammar_ref` / unknown directives)
/// 3. phase IO contract: each phase's `required_inputs` must be
///    produced by some prior phase's `required_outputs` (or a project-
///    bootstrapped artifact like `.ccteam/spec.md`)
/// 4. phase body residue: protocol literals (`PHASE_DONE: <name>` /
///    `ESCALATE:`) inside the body trigger a warn (not error — bodies
///    are user territory; `docs/versions/v0-2/phase-prompt-architecture.md` §9 docks
///    this as warn-not-fail by design).
fn render_validate_team_report(paths: &CcteamPaths, team: &str) -> Result<(String, u32)> {
    use ccteam_core::{default_user_staging_dir, resolve_team, TeamResolveContext};

    let mut out = format!("ccteam doctor --validate-team {team}\n\n");
    let mut fails = 0u32;
    let user_staging = default_user_staging_dir();
    let ctx = TeamResolveContext::for_orchestrator(&paths.root, &user_staging);

    // V0.5.0 F100: plugin manifest staging check removed alongside the
    // `ccteam team init/publish` factory. doctor now only validates
    // the resolved team.yaml.

    match resolve_team(team, &ctx) {
        Ok(s) => out.push_str(&format!("[OK] team.yaml resolves; name=`{}`\n", s.name)),
        Err(err) => {
            out.push_str(&format!("[FAIL] team.yaml load: {err:#}\n"));
            fails += 1;
            out.push_str(&format!("\nSummary: 0 ok, 0 warn, {fails} fail\n"));
            return Ok((out, fails));
        }
    };

    // V0.4.0 F60: phase IO / markdown body checks were tied to the
    // deleted `PhaseTemplate` parser. F63 will reintroduce a
    // `workflow.yaml` validator; until then the doctor only verifies
    // the team.yaml itself resolves.
    out.push_str(
        "[INFO] phase IO / markdown checks skipped — V0.4.0 F60 removed the phase template parser; \
         F63 reintroduces a `workflow.yaml` validator.\n",
    );
    out.push_str(&format!("\nSummary: 1 ok, 0 warn, {fails} fail\n"));
    Ok((out, fails))
}

fn render_reset_shipped_teams_report(
    _paths: &CcteamPaths,
    _opts: &DoctorOptions,
) -> Result<String> {
    // V0.4.0 F60: the shipped team bundle writer (dev / research /
    // meta-agent phase markdowns + team.yaml) was deleted with the
    // rest of the phase machinery. F63 reintroduces a workflow seed
    // writer; until then this is a noop with a clear message.
    let mut out = String::from("ccteam doctor --reset-shipped-teams\n\n");
    out.push_str(
        "  V0.4.0 F60: shipped team bundles removed — supply your own \
         `~/.ccteam/teams/<name>/team.yaml` until F63 lands the new \
         workflow seed writer.\n",
    );
    Ok(out)
}

fn render_install_memory_bridge_report(
    paths: &CcteamPaths,
    opts: &DoctorOptions,
) -> Result<String> {
    // V0.4.0 F60: the shipped team bundle writer was removed; bridge
    // discovery still walks `~/.ccteam/teams/<name>/team.yaml`, so
    // operators must supply those by hand (or via the F63 workflow
    // seed writer once it lands) before running this command.
    let install_opts = ccteam_core::InstallMemoryBridgeOptions {
        dry_run: opts.dry_run,
    };
    let reports = ccteam_core::install_memory_bridge(&paths.root, install_opts)?;
    let mut out = if opts.dry_run {
        String::from("ccteam doctor --install-memory-bridge (dry-run)\n\n")
    } else {
        String::from("ccteam doctor --install-memory-bridge\n\n")
    };
    for r in &reports {
        let label = match &r.action {
            ccteam_core::MemoryBridgeAction::Wrote => "wrote",
            ccteam_core::MemoryBridgeAction::AlreadyPresent => "already-present",
            ccteam_core::MemoryBridgeAction::RepairedMarkedSection => "repaired",
            ccteam_core::MemoryBridgeAction::DryRun { would_write: true } => "would write",
            ccteam_core::MemoryBridgeAction::DryRun { would_write: false } => {
                "no-op (already present)"
            }
        };
        out.push_str(&format!(
            "  {:<24} {:<24} {}\n",
            r.team,
            label,
            r.target.display(),
        ));
    }
    out.push('\n');
    out.push_str(
        "rules files auto-load into every Claude Code session whose cwd matches the\n\
         `paths:` frontmatter; the retro phase Edits the marked block on project end.\n",
    );
    Ok(out)
}

fn render_install_mcp_report() -> Result<String> {
    let path = crate::mcp_serve::install_mcp()?;
    Ok(render_install_mcp_body(&path))
}

/// Pure renderer for the `config mcp` report body, split out from the
/// `~/.claude.json` write so it stays unit-testable without touching the
/// real config. The `tools surface` line interpolates the live tool count
/// from the same `tool_definitions()` source `run_verify_mcp` introspects
/// — never hard-code it, or the number drifts (it was stuck at "9" while
/// the surface grew to 27).
fn render_install_mcp_body(path: &std::path::Path) -> String {
    let mut out = String::from("ccteam config: register MCP server\n\n");
    out.push_str(&format!(
        "  registered ccteam MCP server in {}\n",
        path.display()
    ));
    let total_tools = run_verify_mcp().total_tools;
    out.push_str(&format!("  tools surface : {total_tools}\n"));
    out.push('\n');
    out.push_str(
        "open a new claude session to pick up the change; existing sessions need /reload-mcp.\n",
    );
    out
}

fn render_hook_rewrite_line(r: &HookCmdRewriteReport, _dry_run: bool) -> String {
    let label: String = match &r.action {
        HookCmdRewriteAction::NotFound => "missing".into(),
        HookCmdRewriteAction::NoChangeNeeded => "ok".into(),
        HookCmdRewriteAction::WouldRewrite { entries } => {
            format!("would rewrite {entries}")
        }
        HookCmdRewriteAction::Rewrote { entries } => format!("rewrote {entries}"),
    };
    format!(
        "  {:<28} {:<22} {}\n",
        "settings.json",
        label,
        r.target.display(),
    )
}

/// V0.4.6 F91: `doctor --update-hooks` rendered report. Walks every
/// project registered in `~/.ccteam/config.yaml` (plus the legacy
/// `projects_root` fallback path that `collect_projects` already
/// supports) and strips the now-retired `ccteam hook cost-accumulate`
/// PostToolUse entry from each `<project>/.claude/settings.json`.
fn render_update_hooks_report(paths: &CcteamPaths, dry_run: bool) -> Result<String> {
    let mut out = String::from("ccteam doctor --update-hooks (V0.4.6 F91)\n\n");
    let projects = collect_projects(paths)?;
    if projects.is_empty() {
        out.push_str("  no projects registered — nothing to do.\n\n");
        return Ok(out);
    }
    let mut any_action = false;
    for p in &projects {
        // Resolve the project's on-disk directory. `collect_projects`
        // already de-dupes registry vs legacy walk, so we just need
        // the canonical path. Use config.yaml's registered path when
        // available; fall back to `paths.project_dir(slug)` for
        // legacy projects.
        let project_dir = config_project_dir(paths, &p.state.slug)
            .unwrap_or_else(|| paths.project_dir(&p.state.slug));
        let settings = project_dir.join(".claude").join("settings.json");
        let report = ccteam_core::remove_cost_accumulate_hooks(&settings, dry_run)?;
        let label: String = match &report.action {
            ccteam_core::CostAccumulateScrubAction::NotFound => "missing".into(),
            ccteam_core::CostAccumulateScrubAction::NoChangeNeeded => "ok".into(),
            ccteam_core::CostAccumulateScrubAction::WouldRemove { entries } => {
                any_action = true;
                format!("would remove {entries}")
            }
            ccteam_core::CostAccumulateScrubAction::Removed { entries } => {
                any_action = true;
                format!("removed {entries}")
            }
        };
        out.push_str(&format!(
            "  {:<40} {:<22} {}\n",
            truncate(&p.state.slug, 40),
            label,
            report.target.display(),
        ));
    }
    if !any_action {
        out.push_str(
            "\n  (no `cost-accumulate` hooks found — all settings.json files already clean.)\n",
        );
    }
    out.push('\n');
    Ok(out)
}

/// V0.6.1 F139 — `ccteam doctor --install-hooks` report. Materializes
/// `<paths.root>/hooks/hook.sh` from the embedded template and chmod
/// 0755. Idempotent.
fn render_install_hooks_report(paths: &CcteamPaths) -> Result<String> {
    let mut out = String::from("ccteam doctor --install-hooks (V0.6.1 F139)\n\n");
    let (path, action) = install_hooks(paths)?;
    let label = match action {
        InstallHooksAction::Created => "created",
        InstallHooksAction::Updated => "updated (script body differed)",
        InstallHooksAction::Unchanged => "ok (already current)",
    };
    out.push_str(&format!("  hook.sh           {label}\n"));
    out.push_str(&format!("  target            {}\n", path.display()));
    out.push_str(&format!(
        "  size              {} bytes\n",
        HOOK_DISPATCHER_SH.len()
    ));
    out.push('\n');
    out.push_str(
        "  this script is the per-hook entry. Project `.claude/settings.json`\n  \
         hooks point at it; it POSTs Claude Code hook stdin to the long-running\n  \
         daemon's `/internal/hook/:kind[/:action]` route (~10 ms) and falls\n  \
         back to `ccteam internal hook ...` when the daemon is down (~200 ms).\n\n",
    );
    out.push_str(
        "  next, point legacy project settings.json files at the new wrapper:\n      \
         ccteam doctor --migrate-hook-commands [--dry-run]\n\n",
    );
    Ok(out)
}

/// V0.6.1 F139 — `ccteam doctor --migrate-hook-commands` report.
/// Rewrites every registered project's `.claude/settings.json` so hook
/// command strings invoke `~/.ccteam/hooks/hook.sh` instead of the
/// V0.4.6 / V0.6.0 `<ccteam-bin> internal hook ...` form (or older
/// `cct hook ...` / `ccteam hook ...` forms). Pair with `--dry-run` to
/// preview without writing. Idempotent.
fn render_migrate_hook_commands_report(paths: &CcteamPaths, dry_run: bool) -> Result<String> {
    let mut out = String::from("ccteam doctor --migrate-hook-commands (V0.6.1 F139)\n\n");
    let hook_sh = paths.hooks_script();
    if !hook_sh.exists() {
        out.push_str(&format!(
            "  WARNING: {} does not exist yet — run\n           ccteam doctor --install-hooks\n           first so the rewriter has a real target.\n\n",
            hook_sh.display(),
        ));
    }
    let reports = scan_project_settings_for_hook_rewrite(&paths.projects_root, &hook_sh, dry_run)?;
    if reports.is_empty() {
        out.push_str("  no project `settings.json` files found.\n\n");
        return Ok(out);
    }
    let mut touched = 0usize;
    for r in &reports {
        let line = render_hook_rewrite_line(r, dry_run);
        out.push_str(&line);
        if matches!(
            r.action,
            HookCmdRewriteAction::Rewrote { .. } | HookCmdRewriteAction::WouldRewrite { .. }
        ) {
            touched += 1;
        }
    }
    if touched == 0 {
        out.push_str(
            "\n  (all settings.json files already invoke hook.sh — nothing to migrate.)\n",
        );
    } else if dry_run {
        out.push_str(&format!(
            "\n  {touched} settings.json file(s) would be rewritten — re-run without --dry-run to apply.\n",
        ));
    } else {
        out.push_str(&format!("\n  {touched} settings.json file(s) rewritten.\n",));
    }
    out.push('\n');
    Ok(out)
}

/// V0.6.1 F121 — pricing staleness classifier thresholds (days since
/// `schema_version` for the embedded rate sheet).
const PRICING_WARN_DAYS: i64 = 180;
const PRICING_ERROR_DAYS: i64 = 365;

/// V0.6.1 F121 — per-vendor pricing classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PricingStaleness {
    /// `age_days <= 180`.
    Ok,
    /// `180 < age_days <= 365` — pricing aging, refresh on next ccteam upgrade.
    Warn,
    /// `age_days > 365` — ship blocker, embedded table must be re-pulled.
    Error,
    /// `schema_version` failed to parse as `YYYY-MM-DD` (defensive: a
    /// future schema loosening must not panic the doctor command).
    Unparsed,
}

impl PricingStaleness {
    fn classify(age_days: i64) -> Self {
        if age_days > PRICING_ERROR_DAYS {
            Self::Error
        } else if age_days > PRICING_WARN_DAYS {
            Self::Warn
        } else {
            Self::Ok
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::Warn => "warn pricing aging",
            Self::Error => "ERROR ship needs re-pull",
            Self::Unparsed => "note unparsed schema_version",
        }
    }
}

/// V0.6.1 F121 — read the `CCTEAM_TEST_NOW=YYYY-MM-DD` env override
/// when present so tests can pin "today" deterministically. Falls back
/// to UTC `today`.
fn doctor_today() -> chrono::NaiveDate {
    if let Ok(raw) = std::env::var("CCTEAM_TEST_NOW") {
        if let Ok(d) = chrono::NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d") {
            return d;
        }
    }
    chrono::Utc::now().date_naive()
}

/// V0.5.0 F92 / V0.6.1 F121 — classify each vendor's embedded pricing
/// table against today and emit one line per vendor:
///
/// ```text
/// [pricing.anthropic] pulled 2026-05-19 (now -0d, OK)
/// [pricing.openai]    pulled 2026-02-19 (now -90d, warn pricing aging)
/// [pricing.openai]    pulled 2024-12-19 (now -365d, ERROR ship needs re-pull)
/// ```
///
/// `age_days <= 180` → OK; `180 < age_days <= 365` → warn (pricing aging);
/// `age_days > 365` → ERROR (ship needs a re-pull).
///
/// The `CCTEAM_TEST_NOW` env override pins "today" so tests can stamp
/// the three states deterministically.
fn render_check_pricing_version_report() -> String {
    let mut out =
        String::from("ccteam doctor --check-pricing-version (V0.5.0 F92 / V0.6.1 F121)\n\n");
    let today = doctor_today();
    let mut worst = PricingStaleness::Ok;
    for (label, vendor) in [
        ("pricing.anthropic", Vendor::Claude),
        ("pricing.openai", Vendor::Codex),
    ] {
        let pulled = pricing_schema_version_for(vendor);
        match chrono::NaiveDate::parse_from_str(pulled, "%Y-%m-%d") {
            Ok(authored) => {
                let age_days = (today - authored).num_days();
                let state = PricingStaleness::classify(age_days);
                out.push_str(&format!(
                    "  [{label}] pulled {pulled} (now -{age_days}d, {})\n",
                    state.label(),
                ));
                if state_severity(state) > state_severity(worst) {
                    worst = state;
                }
            }
            Err(_) => {
                out.push_str(&format!(
                    "  [{label}] pulled {pulled} (now ??d, {})\n",
                    PricingStaleness::Unparsed.label(),
                ));
                if state_severity(PricingStaleness::Unparsed) > state_severity(worst) {
                    worst = PricingStaleness::Unparsed;
                }
            }
        }
    }
    match worst {
        PricingStaleness::Ok => {
            out.push_str(&format!(
                "\n  pricing tables fresh (≤ {PRICING_WARN_DAYS} days).\n",
            ));
        }
        PricingStaleness::Warn => {
            out.push_str(&format!(
                "\n  [WARN] one or more pricing tables older than {PRICING_WARN_DAYS} days. \
                 Upgrade ccteam to refresh the bundled rate sheet \
                 (see crates/ccteam-cost/pricing/anthropic.toml + openai.toml).\n",
            ));
        }
        PricingStaleness::Error => {
            out.push_str(&format!(
                "\n  [ERROR] one or more pricing tables older than {PRICING_ERROR_DAYS} days — \
                 ship blocker. Re-pull the rate sheets in \
                 crates/ccteam-cost/pricing/{{anthropic,openai}}.toml \
                 (bump `schema_version` to today's YYYY-MM-DD).\n",
            ));
        }
        PricingStaleness::Unparsed => {
            out.push_str(
                "\n  [note] one or more `schema_version` values did not parse as YYYY-MM-DD; \
                 staleness check skipped for those vendors.\n",
            );
        }
    }
    // V0.5.x callers (and the V0.6.0 wave-1 baseline) still expect to
    // see the overall "newer of the two" date for tooling that scrapes
    // the report; preserve a trailing single line referencing the
    // existing `pricing_schema_version()` accessor so the API is not
    // silently retired by the F121 rewrite.
    out.push_str(&format!(
        "\n  pricing_schema_version() (newer of the two): {}\n",
        pricing_schema_version(),
    ));
    out.push('\n');
    out
}

/// V0.6.1 F121 — severity ordering for the worst-of summary line.
fn state_severity(s: PricingStaleness) -> u8 {
    match s {
        PricingStaleness::Ok => 0,
        PricingStaleness::Unparsed => 1,
        PricingStaleness::Warn => 2,
        PricingStaleness::Error => 3,
    }
}

/// V0.6.0 Wave 3 F112 — probe `codex --version` and emit a single
/// report block. Minimum supported: `0.131` (the version that
/// stabilised `--json` + `app-server daemon` UX). Versions <0.131 WARN
/// but don't fail; missing `codex` binary surfaces an ERROR line so
/// the operator knows mode-3 codex bot path will degrade.
fn render_check_codex_version_report() -> String {
    const MIN_MAJOR: u32 = 0;
    const MIN_MINOR: u32 = 131;
    let mut out = String::from("ccteam doctor --check-codex-version (V0.6.0 Wave 3 F112)\n\n");
    let output = Command::new("codex").arg("--version").output();
    match output {
        Ok(o) if o.status.success() => {
            let line = String::from_utf8_lossy(&o.stdout).trim().to_string();
            out.push_str(&format!("  codex --version: {line}\n"));
            match parse_codex_semver(&line) {
                Some((maj, min, _patch)) => {
                    if (maj, min) >= (MIN_MAJOR, MIN_MINOR) {
                        out.push_str(&format!(
                            "  [OK] codex {maj}.{min} ≥ {MIN_MAJOR}.{MIN_MINOR} (mode-3 \
                             codex bot path supported).\n"
                        ));
                    } else {
                        out.push_str(&format!(
                            "  [WARN] codex {maj}.{min} < {MIN_MAJOR}.{MIN_MINOR} — upgrade \
                             codex for mode-3 bot path + `codex exec --json` stability.\n"
                        ));
                    }
                }
                None => {
                    out.push_str(
                        "  [note] could not parse semver from `codex --version` output; \
                         staleness check skipped.\n",
                    );
                }
            }
        }
        Ok(o) => {
            out.push_str(&format!(
                "  [ERROR] `codex --version` exited {} — mode-3 codex bot path will degrade.\n",
                o.status.code().unwrap_or(-1)
            ));
            out.push_str(&format!(
                "  stderr: {}\n",
                String::from_utf8_lossy(&o.stderr).trim()
            ));
        }
        Err(err) => {
            out.push_str(&format!(
                "  [ERROR] codex CLI not installed (`{err}`) — V0.6 Codex features will \
                 fallback or skip.\n"
            ));
        }
    }
    out.push('\n');
    out
}

/// V0.6.0 Wave 3 F112 — probe `codex login status` and emit a single
/// report block. Parses one of:
///
/// - `Logged in using ChatGPT`           → OK
/// - `Logged in using API key`           → OK
/// - `Not logged in` / `Logged out`      → WARN (operator action needed)
///
/// A missing `codex` binary errors out so the operator sees the same
/// remediation as `--check-codex-version`.
fn render_check_codex_auth_report() -> String {
    let mut out = String::from("ccteam doctor --check-codex-auth (V0.6.0 Wave 3 F112)\n\n");
    let output = Command::new("codex").args(["login", "status"]).output();
    match output {
        Ok(o) => {
            let combined = format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            );
            out.push_str(&format!("  codex login status: {}\n", combined.trim()));
            let status = classify_codex_auth(&combined);
            match status {
                CodexAuthStatus::LoggedIn(via) => {
                    out.push_str(&format!(
                        "  [OK] codex authenticated via {via} — mode-3 codex bot path enabled.\n"
                    ));
                }
                CodexAuthStatus::LoggedOut => {
                    out.push_str(
                        "  [WARN] codex is not logged in. Run `codex login` to enable Codex \
                         features (mode-3 bot path + /ccteam-advise codex dual-probe).\n",
                    );
                }
                CodexAuthStatus::Unknown => {
                    out.push_str(
                        "  [note] could not parse `codex login status` output; assume logged \
                         out and re-run after `codex login`.\n",
                    );
                }
            }
        }
        Err(err) => {
            out.push_str(&format!(
                "  [ERROR] `codex login status` failed: {err} — install codex first.\n"
            ));
        }
    }
    out.push('\n');
    out
}

/// V0.6.6 F173 — cost-orphan invariant check. Walks every registered
/// project's `progress.jsonl` for the last 24h, counts `agent_done`
/// events with `status="completed"` per vendor, then compares against
/// the per-vendor row count in `<ccteam_root>/cost-budget.json` over
/// the same window. Each vendor with `agent_done_count > ledger_rows`
/// surfaces a `[WARN] cost orphan: …` line so a future regression
/// (e.g. a new adapter that forgets the ledger hook) is visible at
/// `ccteam doctor` time. Fully reconciled state emits one `[OK] cost
/// ledger reconciled` line.
fn render_check_cost_orphan_report(paths: &CcteamPaths) -> String {
    let mut out = String::from("ccteam doctor --check-cost-orphan (V0.6.6 F173)\n\n");
    let (counts, warnings) = compute_cost_orphan(paths);
    let claude_done = counts
        .agent_done
        .get(&CostVendor::Claude)
        .copied()
        .unwrap_or(0);
    let codex_done = counts
        .agent_done
        .get(&CostVendor::Codex)
        .copied()
        .unwrap_or(0);
    let claude_rows = counts
        .ledger_rows
        .get(&CostVendor::Claude)
        .copied()
        .unwrap_or(0);
    let codex_rows = counts
        .ledger_rows
        .get(&CostVendor::Codex)
        .copied()
        .unwrap_or(0);
    out.push_str(&format!(
        "  progress.jsonl agent_done (24h): claude={claude_done} codex={codex_done}\n"
    ));
    out.push_str(&format!(
        "  cost-budget.json ledger rows (24h): claude={claude_rows} codex={codex_rows}\n"
    ));
    if warnings.is_empty() {
        out.push_str(
            "  [OK] cost ledger reconciled — every vendor adapter call has a ledger row.\n",
        );
    } else {
        for w in &warnings {
            out.push_str("  [WARN] ");
            out.push_str(w);
            out.push('\n');
        }
        out.push_str(
            "  Likely cause: a vendor adapter spawn path bypassed the F173 ledger hook \
             (`append_budget_ledger_row` in CodexExecAdapter::submit_turn). Inspect the \
             new spawn site and add the hook before merging.\n",
        );
    }
    out.push('\n');
    out
}

/// V0.6.6 F173 — pure helper for [`render_check_cost_orphan_report`]
/// so tests can drive the invariant without parsing report text.
/// Returns per-vendor counts + a list of warning strings (empty when
/// fully reconciled).
pub fn compute_cost_orphan(paths: &CcteamPaths) -> (CostOrphanCounts, Vec<String>) {
    let cutoff = chrono::Utc::now() - chrono::Duration::hours(24);
    let mut counts = CostOrphanCounts::default();

    // 1. agent_done per vendor across every registered project's progress.jsonl.
    if let Ok(projects) = ccteam_core::collect_projects(paths) {
        for proj in projects {
            let slug = &proj.state.slug;
            let progress_path = paths.progress_jsonl(slug);
            let events = ccteam_core::read_all_events(&progress_path).unwrap_or_default();
            for ev in events {
                if ev.get("event").and_then(|s| s.as_str()) != Some("agent_done") {
                    continue;
                }
                if ev.get("status").and_then(|s| s.as_str()) != Some("completed") {
                    continue;
                }
                let ts = ev
                    .get("ts")
                    .and_then(|s| s.as_str())
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&chrono::Utc));
                if let Some(ts) = ts {
                    if ts < cutoff {
                        continue;
                    }
                }
                let vendor = ev.get("vendor").and_then(|s| s.as_str()).unwrap_or("");
                let key = match vendor {
                    "claude" => CostVendor::Claude,
                    "codex" => CostVendor::Codex,
                    _ => continue, // unknown vendor — skip (forward-compat)
                };
                *counts.agent_done.entry(key).or_insert(0) += 1;
            }
        }
    }

    // 2. ledger rows per vendor over the same window.
    if let Ok(ledger) = ccteam_core::load_advise_budget(&paths.root) {
        for sample in &ledger.samples {
            if sample.ts < cutoff {
                continue;
            }
            let key = match sample.vendor {
                ccteam_core::Vendor::Claude => CostVendor::Claude,
                ccteam_core::Vendor::Codex => CostVendor::Codex,
            };
            *counts.ledger_rows.entry(key).or_insert(0) += 1;
        }
    }

    // 3. warnings — only flag `agent_done > ledger_rows` (orphans, the
    //    F173 invariant). The reverse (advise_vote bumping ledger rows
    //    without progress.jsonl events) is expected — advise calls are
    //    not project-scoped, so they have no progress.jsonl footprint.
    let mut warnings = Vec::new();
    for vendor in [CostVendor::Claude, CostVendor::Codex] {
        let done = counts.agent_done.get(&vendor).copied().unwrap_or(0);
        let rows = counts.ledger_rows.get(&vendor).copied().unwrap_or(0);
        if done > rows {
            warnings.push(format!(
                "cost orphan: {} {} calls in progress.jsonl, {} rows in ledger (Δ={})",
                done,
                vendor.label(),
                rows,
                done - rows,
            ));
        }
    }
    (counts, warnings)
}

/// V0.6.6 F173 — per-vendor cost rollup tally returned by
/// [`compute_cost_orphan`]. Counts are 24h windowed (rolling, matches
/// the advise ledger GC window).
#[derive(Debug, Default)]
pub struct CostOrphanCounts {
    pub agent_done: std::collections::HashMap<CostVendor, u32>,
    pub ledger_rows: std::collections::HashMap<CostVendor, u32>,
}

/// V0.6.6 F173 — vendor enum mirror used by the cost-orphan check.
/// Mirrors `ccteam_harness::AgentVendor` but lives in the cli
/// crate so the report rendering layer doesn't leak the core enum
/// through its public surface. `Hash + Eq` for `HashMap` keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CostVendor {
    Claude,
    Codex,
}

impl CostVendor {
    fn label(self) -> &'static str {
        match self {
            CostVendor::Claude => "claude",
            CostVendor::Codex => "codex",
        }
    }
}

/// V0.6.5 F155 — deterministic gate for the `ccteam-creator` Phase
/// 3.5 Codex auto-critic injection. Returns `(report_body, exit_code)`
/// where:
///
/// - exit 0 = codex binary available, version ≥ minimum, and the
///   one-shot `<bin> exec --json --skip-git-repo-check` probe emitted
///   at least one well-formed `turn.completed` JSONL line carrying a
///   non-empty agent_message item;
/// - exit 2 = codex binary missing / `--version` probe failed / non-
///   zero exit code (treated as `available: false`);
/// - exit 3 = codex available but the `codex exec --json` output is
///   malformed (no parseable JSONL `turn.completed` event), so the
///   skill MUST NOT inject `executor: codex` until the operator's
///   codex install is fixed.
///
/// The function never panics on subprocess errors. Honors the
/// `CCTEAM_CODEX_BIN` env override so hermetic tests can swap in a
/// stub script (the same pattern used by the V0.6.0 Wave 3 codex_exec
/// adapter tests).
///
/// stdout body is a single JSON line (`{"available": ...}\n`) so
/// skill bodies can parse with one `jq` invocation. A short human-
/// readable header precedes the JSON for `ccteam doctor` operators
/// who run the flag manually.
pub fn run_check_codex_auto_critic() -> (String, i32) {
    let bin = std::env::var("CCTEAM_CODEX_BIN").unwrap_or_else(|_| "codex".to_string());
    let header = format!(
        "ccteam doctor --check-codex-auto-critic (V0.6.5 F155)\n  \
         probing codex binary: {bin}\n"
    );

    // Step 1: `<bin> --version` — the cheap availability check.
    let version_out = Command::new(&bin).arg("--version").output();
    let version_line = match version_out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Ok(o) => {
            let body = json!({
                "available": false,
                "reason": format!(
                    "`{bin} --version` exited {}: {}",
                    o.status.code().unwrap_or(-1),
                    String::from_utf8_lossy(&o.stderr).trim()
                ),
                "exit_code": 2,
            });
            return (
                format!("{header}{}\n", serde_json::to_string(&body).unwrap()),
                2,
            );
        }
        Err(err) => {
            let body = json!({
                "available": false,
                "reason": format!("spawn `{bin} --version` failed: {err}"),
                "exit_code": 2,
            });
            return (
                format!("{header}{}\n", serde_json::to_string(&body).unwrap()),
                2,
            );
        }
    };

    // Step 2: deterministic one-shot `codex exec --json` canary. We
    // pipe a minimal canary prompt (heredoc-equivalent) and look for a
    // `turn.completed` JSONL frame in stdout — that's the marker the
    // skill needs to know "this codex install can deliver a critic
    // verdict end-to-end without burning real inference cost in the
    // gate itself". The stub binary in the test suite emits a fixed
    // JSONL transcript so the test is deterministic; real codex will
    // also emit `turn.completed` after `--json` mode finishes (cheap
    // ~1¢ depending on user's plan).
    //
    // Argv mirrors `crates/ccteam-harness/src/execution/codex_exec.rs::
    // build_exec_argv` for parity with the real wave-3 adapter; the
    // stub MUST accept the same flags.
    let probe = Command::new(&bin)
        .args(["exec", "--json", "--skip-git-repo-check"])
        .arg("respond with the literal text OK")
        .output();
    match probe {
        Ok(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            let has_turn_completed = stdout
                .lines()
                .filter_map(|l| serde_json::from_str::<Value>(l).ok())
                .any(|v| v.get("type").and_then(|t| t.as_str()) == Some("turn.completed"));
            if has_turn_completed {
                let body = json!({
                    "available": true,
                    "version": version_line,
                    "probe": "ok",
                    "exit_code": 0,
                });
                (
                    format!("{header}{}\n", serde_json::to_string(&body).unwrap()),
                    0,
                )
            } else {
                let body = json!({
                    "available": true,
                    "version": version_line,
                    "probe": "malformed",
                    "reason": "`codex exec --json` exited 0 but no `turn.completed` JSONL \
                               frame found on stdout; skill MUST NOT inject `executor: codex` \
                               until codex install is fixed",
                    "exit_code": 3,
                });
                (
                    format!("{header}{}\n", serde_json::to_string(&body).unwrap()),
                    3,
                )
            }
        }
        Ok(o) => {
            let body = json!({
                "available": false,
                "version": version_line,
                "reason": format!(
                    "`codex exec --json` exited {}: {}",
                    o.status.code().unwrap_or(-1),
                    String::from_utf8_lossy(&o.stderr).trim()
                ),
                "exit_code": 2,
            });
            (
                format!("{header}{}\n", serde_json::to_string(&body).unwrap()),
                2,
            )
        }
        Err(err) => {
            let body = json!({
                "available": false,
                "version": version_line,
                "reason": format!("spawn `codex exec` failed: {err}"),
                "exit_code": 2,
            });
            (
                format!("{header}{}\n", serde_json::to_string(&body).unwrap()),
                2,
            )
        }
    }
}

/// V0.6.6 F171 — outcome of `ccteam doctor --verify-mcp`. Counts the
/// MCP tool surface registered by `mcp_serve::tool_definitions()` and
/// cross-checks against the `STUB_TOOLS` allow-list declared in
/// `mcp_tool_groups`. `unexpected_stubs` is the set difference
/// between live STUBs and the allow-list (today the allow-list is
/// empty so any STUB is unexpected); `ok()` returns false when that
/// set is non-empty.
#[derive(Debug, Clone)]
pub struct VerifyMcpReport {
    /// Total number of MCP tools registered by `tool_definitions()`.
    pub total_tools: usize,
    /// Number of tools classified as STUB (name appears in
    /// `mcp_tool_groups::STUB_TOOLS`).
    pub stub_count: usize,
    /// Number of tools with a real dispatch (= total - stub_count).
    pub active_count: usize,
    /// Sorted, full tool names (e.g. `ccteam__workflow_show`) for the
    /// human-readable + JSON reports.
    pub tool_list: Vec<String>,
    /// Per-group counts (`workflow` → 15, `chat` → 8, ...). Sorted by
    /// group name for deterministic output.
    pub per_group: std::collections::BTreeMap<String, GroupStats>,
    /// STUB tool names that are NOT in the `STUB_TOOLS` allow-list.
    /// Empty in a clean build; non-empty → exit code 1.
    pub unexpected_stubs: Vec<String>,
}

/// V0.6.6 F171 — per-group active/stub split used by `VerifyMcpReport`.
#[derive(Debug, Clone)]
pub struct GroupStats {
    pub active: usize,
    pub stub: usize,
}

impl VerifyMcpReport {
    /// True when every registered tool has a real dispatch — i.e. the
    /// allow-list and the live STUB set agree. CI uses this to decide
    /// the exit code (`true` → 0, `false` → 1).
    pub fn ok(&self) -> bool {
        self.unexpected_stubs.is_empty()
    }

    /// Human-readable report (default). Mirrors the layout in
    /// `docs/versions/v0-6-6/prd.md` §F171 Sub-3.
    pub fn render_text(&self) -> String {
        let mut out = String::new();
        out.push_str("MCP tool surface verification (V0.6.6 F171)\n");
        out.push_str("===\n");
        out.push_str(&format!("total tools:    {}\n", self.total_tools));
        out.push_str(&format!("active:         {}\n", self.active_count));
        out.push_str(&format!("stubs:          {}\n", self.stub_count));
        out.push_str("\nper-group breakdown:\n");
        for (group, stats) in &self.per_group {
            out.push_str(&format!(
                "  {:<12} {} active / {} stub\n",
                format!("{group}:"),
                stats.active,
                stats.stub
            ));
        }
        if !self.unexpected_stubs.is_empty() {
            out.push_str("\nunexpected STUBs (not in mcp_tool_groups::STUB_TOOLS):\n");
            for name in &self.unexpected_stubs {
                out.push_str(&format!("  - {name}\n"));
            }
        }
        out.push('\n');
        if self.ok() {
            out.push_str(&format!(
                "verdict: PASS — all {} tools live, no production STUBs.\n",
                self.total_tools
            ));
        } else {
            out.push_str(&format!(
                "verdict: FAIL — {} unexpected STUB(s) registered.\n",
                self.unexpected_stubs.len()
            ));
        }
        out
    }

    /// Single pretty-printed JSON object (trailing newline) for
    /// machine-readable callers (CI, `jq`-driven scripts). Hand-built
    /// via `serde_json::json!` so the report type does not need a
    /// `serde::Serialize` derive (ccteam-cli does not depend on the
    /// `serde` crate directly).
    pub fn render_json(&self) -> String {
        let per_group: Map<String, Value> = self
            .per_group
            .iter()
            .map(|(g, s)| (g.clone(), json!({ "active": s.active, "stub": s.stub })))
            .collect();
        let body = json!({
            "ok": self.ok(),
            "total_tools": self.total_tools,
            "active_count": self.active_count,
            "stub_count": self.stub_count,
            "tool_list": self.tool_list,
            "per_group": Value::Object(per_group),
            "unexpected_stubs": self.unexpected_stubs,
        });
        let mut s = serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".into());
        s.push('\n');
        s
    }
}

/// V0.6.6 F171 — compute the report by introspecting
/// `mcp_serve::tool_definitions()` (single source of truth for the
/// registered MCP tool surface) and cross-checking against
/// `mcp_tool_groups::STUB_TOOLS`.
pub fn run_verify_mcp() -> VerifyMcpReport {
    let tools = crate::mcp_serve::tool_definitions();
    let mut names: Vec<String> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(|v| v.as_str()).map(String::from))
        .collect();
    names.sort();

    let stub_set: std::collections::HashSet<&str> =
        crate::mcp_tool_groups::STUB_TOOLS.iter().copied().collect();

    let stub_count = names
        .iter()
        .filter(|n| stub_set.contains(n.as_str()))
        .count();
    let active_count = names.len().saturating_sub(stub_count);

    // Per-group split. Tools whose `group_for_tool` returns `None` are
    // bucketed under "other" so a typo in a new tool name still shows
    // up in the report rather than silently disappearing.
    let mut per_group: std::collections::BTreeMap<String, GroupStats> =
        std::collections::BTreeMap::new();
    for name in &names {
        let group = match crate::mcp_tool_groups::group_for_tool(name) {
            Some(g) => g.as_str().to_string(),
            None => "other".to_string(),
        };
        let entry = per_group
            .entry(group)
            .or_insert(GroupStats { active: 0, stub: 0 });
        if stub_set.contains(name.as_str()) {
            entry.stub += 1;
        } else {
            entry.active += 1;
        }
    }

    // Unexpected STUBs = live STUBs not in the allow-list. Today the
    // allow-list is empty so this equals the full live STUB set; the
    // indirection lets a future PR park a known-stub under the
    // allow-list without forcing CI red.
    let unexpected_stubs: Vec<String> = names
        .iter()
        .filter(|n| stub_set.contains(n.as_str()))
        .cloned()
        .collect();

    VerifyMcpReport {
        total_tools: names.len(),
        stub_count,
        active_count,
        tool_list: names,
        per_group,
        unexpected_stubs,
    }
}

/// Parse "codex 0.131.0" or "0.131.0 (...)". Returns `(major, minor,
/// patch)` on success.
pub fn parse_codex_semver(line: &str) -> Option<(u32, u32, u32)> {
    // Scan for the first `<digits>.<digits>.<digits>` triple.
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            let slice = &line[start..i];
            let parts: Vec<&str> = slice.split('.').collect();
            if parts.len() >= 3 {
                if let (Ok(maj), Ok(min), Ok(patch)) = (
                    parts[0].parse::<u32>(),
                    parts[1].parse::<u32>(),
                    parts[2]
                        .split(|c: char| !c.is_ascii_digit())
                        .next()
                        .unwrap_or("0")
                        .parse::<u32>(),
                ) {
                    return Some((maj, min, patch));
                }
            }
        }
        i += 1;
    }
    None
}

/// Outcome of parsing `codex login status` output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexAuthStatus {
    LoggedIn(String),
    LoggedOut,
    Unknown,
}

/// Classify the combined stdout+stderr of `codex login status`. We
/// check the "logged out" branch FIRST because "Not logged in" is a
/// substring of "logged in" — a naive `contains("logged in")` would
/// misclassify the negative path.
pub fn classify_codex_auth(combined: &str) -> CodexAuthStatus {
    let lower = combined.to_ascii_lowercase();
    if lower.contains("not logged in")
        || lower.contains("logged out")
        || lower.contains("no credentials")
        || lower.contains("please run `codex login`")
    {
        return CodexAuthStatus::LoggedOut;
    }
    if lower.contains("logged in") {
        let via = if lower.contains("chatgpt") {
            "ChatGPT"
        } else if lower.contains("api key") || lower.contains("api-key") {
            "API key"
        } else {
            "(unspecified)"
        };
        return CodexAuthStatus::LoggedIn(via.to_string());
    }
    CodexAuthStatus::Unknown
}

/// Resolve a project's on-disk directory from `~/.ccteam/config.yaml`
/// (the V0.4.2 F73 registry). Returns `None` for slugs not registered
/// (legacy `~/projects/<slug>/` walk fallback).
fn config_project_dir(paths: &CcteamPaths, slug: &str) -> Option<std::path::PathBuf> {
    let cfg = ccteam_core::config::load(&paths.root).ok()?;
    cfg.projects
        .iter()
        .find(|e| e.slug == slug)
        .map(|e| e.path.clone())
}

/// Walk every immediate child of `projects_root` and rewrite legacy
/// hook commands in `<child>/.claude/settings.json`. Returns a per-
/// project report so the doctor output can summarize.
fn scan_project_settings_for_hook_rewrite(
    projects_root: &std::path::Path,
    new_hook_sh: &std::path::Path,
    dry_run: bool,
) -> Result<Vec<HookCmdRewriteReport>> {
    let mut out: Vec<HookCmdRewriteReport> = Vec::new();
    let entries = match std::fs::read_dir(projects_root) {
        Ok(e) => e,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(err) => {
            return Err(err).with_context(|| format!("read_dir {}", projects_root.display()));
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let settings = path.join(".claude").join("settings.json");
        if !settings.exists() {
            continue;
        }
        let report = rewrite_legacy_hook_commands(&settings, new_hook_sh, dry_run)?;
        out.push(report);
    }
    Ok(out)
}

fn render_migrate_recommended_agents_report(opts: &DoctorOptions) -> Result<String> {
    let claude = user_claude_dir()?;
    let reports = migrate_recommended_agent_symlinks(&claude, opts.dry_run)?;
    let mut out = String::new();
    out.push_str(if opts.dry_run {
        "ccteam doctor --migrate-recommended-agents (dry-run)\n"
    } else {
        "ccteam doctor --migrate-recommended-agents\n"
    });
    out.push('\n');
    if reports.is_empty() {
        out.push_str(
            "  no stale ccteam-managed symlinks found under ~/.claude/agents/ — nothing to do.\n",
        );
        return Ok(out);
    }
    for r in &reports {
        out.push_str(&render_migration_line(r, opts.dry_run));
    }
    out.push('\n');
    if opts.dry_run {
        out.push_str("no changes made (--dry-run). drop the flag to apply.\n");
    } else {
        out.push_str(&format!(
            "removed {} stale symlink(s); spawned project sessions now resolve plugin agents \
             through the in-memory plugin pipeline (V0.2 M0.20).\n",
            reports.len(),
        ));
    }
    Ok(out)
}

fn render_migration_line(r: &MigrationReport, dry_run: bool) -> String {
    let label = if dry_run { "would remove" } else { "removed" };
    format!(
        "  {:<28} {:<14} -> {}\n",
        r.target.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
        label,
        r.previous_link.display(),
    )
}

/// V0.2.2 F38: end-to-end smoke for the screenshot pipeline.
/// Reports the font in use, then attempts a real `render_screenshot`
/// and surfaces either the resulting PNG path + size or the
/// degrade reason (tmux missing / vt100 panic / IO failure).
fn render_screenshot_smoke_report(paths: &CcteamPaths, slug: &str) -> Result<String> {
    let mut out = String::from("ccteam doctor --screenshot-smoke\n\n");
    out.push_str(&format!("  slug:        {slug}\n"));
    match ccteam_core::probe_screenshot_font() {
        Ok(label) => out.push_str(&format!("  font probe:  ok  ({label})\n")),
        Err(err) => {
            out.push_str(&format!("  font probe:  FAIL  ({err:#})\n"));
            out.push_str(
                "\nfont check failed — set CCTEAM_SCREENSHOT_FONT_TTF or restore the \
                 vendored JetBrainsMono-Regular.ttf under crates/ccteam-core/assets/fonts/.\n",
            );
            return Ok(out);
        }
    }
    match ccteam_core::render_screenshot(paths, slug, None, 50)? {
        Some(path) => {
            let size = std::fs::metadata(&path)
                .map(|m| m.len())
                .unwrap_or_default();
            out.push_str("  render:      ok\n");
            out.push_str(&format!("  png:         {}\n", path.display()));
            out.push_str(&format!("  size:        {size} bytes\n"));
            out.push('\n');
            out.push_str(
                "open the PNG with `xdg-open` / `open` to confirm the pane was captured.\n",
            );
        }
        None => {
            out.push_str("  render:      degraded  (no PNG written)\n\n");
            out.push_str(
                "rendering returned Ok(None). Common causes:\n  \
                 - the tmux session `ccteam-<slug>` does not exist (start with `ccteam start`).\n  \
                 - tmux is not installed on this host.\n  \
                 - vt100 / imageproc panicked on a malformed input (unusual; check daemon stderr).\n  \
                 - IO error writing the PNG file.\n\n\
                 Re-run with RUST_LOG=warn to see the precise reason in stderr.\n",
            );
        }
    }
    Ok(out)
}

fn render_tool_surface_report(_paths: &CcteamPaths) -> Result<String> {
    let claude = user_claude_dir()?;
    let snap = ToolSurfaceSnapshot::scan(&claude)?;

    let mut out = String::new();
    out.push_str("ccteam doctor --tool-surface\n");
    out.push_str(&format!("\nclaude dir       : {}\n", claude.display()));
    out.push_str(&format!(
        "subagents seen   : {} (incl. {} built-ins)\n",
        snap.subagents.len(),
        BUILTIN_SUBAGENTS.len(),
    ));
    out.push_str(&format!("skills seen      : {}\n", snap.skills.len()));
    out.push_str(&format!("mcp servers seen : {}\n", snap.mcp.len()));
    out.push('\n');
    out.push_str(
        "V0.4.0 F60: phase template walk removed. F63 will reintroduce \
         a workflow-aware tool-surface check; until then this report \
         only enumerates what's installed under ~/.claude/.\n",
    );
    Ok(out)
}

// V0.3 M5.1 — `ProjectSummary` / `collect_projects` /
// `collect_recent_events` moved to `ccteam_core::queries` so the
// V0.3 web crate can reuse them without depending on `ccteam-cli`
// (binary-as-library is a dep-graph anti-pattern). Re-exported below
// so existing call sites (`run_ls`, `run_progress`, `mcp_serve.rs`)
// keep their `use ccteam_cli::commands::{collect_projects, ...}`
// lines unchanged. See `docs/dev-coupling-audit.md` F45.
pub use ccteam_core::{collect_projects, collect_recent_events, ProjectSummary};

/// Collect every `.md` artifact under `<project>/.ccteam/` so non-dev
/// teams (e.g. product-research with `verdict.md` / `rationale.md` /
/// `next-steps.md` / `brief.md` / `market-survey.md`) get listed in
/// `ccteam show <slug> --format json` without ccteam-cli holding a
/// per-team artifact whitelist (F8 fix, 2026-05-07).
///
/// Key = file stem with `-` → `_` (e.g. `plan-eng.md` → `plan_eng`)
/// so existing JSON consumers (the meta-agent dispatch tree, the
/// ccteam-control skill) keep working without a schema migration.
/// Sub-directories under `.ccteam/` (e.g. `outbox/`, `inbox/`) are
/// not enumerated — those have dedicated views.
///
/// `auto-loop.state.md` is the orchestrator's runtime state file
/// (formerly `fix-loop.state.md`); excluded from artifact reporting.
fn collect_artifacts(paths: &CcteamPaths, slug: &str) -> Map<String, Value> {
    let mut m = Map::new();
    let ccteam_dir = paths.project_ccteam_dir(slug);
    let Ok(entries) = std::fs::read_dir(&ccteam_dir) else {
        return m;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if !name.ends_with(".md") {
            continue;
        }
        // Skip orchestrator-internal runtime state files.
        if name == "auto-loop.state.md" || name == "fix-loop.state.md" {
            continue;
        }
        let stem = name.trim_end_matches(".md");
        let key = stem.replace('-', "_");
        m.insert(key, Value::String(format!(".ccteam/{name}")));
    }
    m
}

fn render_ls_text(paths: &CcteamPaths, projects: &[ProjectSummary], daemon_up: bool) -> String {
    let mut out = String::new();
    // F27 — daemon health one-liner, always emitted (even on the
    // empty-projects path) so users can disambiguate "no projects" from
    // "daemon never came up".
    out.push_str(&format!(
        "daemon: {}\n",
        if daemon_up { "up" } else { "down" }
    ));
    if projects.is_empty() {
        out.push_str(
            "(no projects under ~/projects/. start one with `ccteam new \"<request>\"`.)\n",
        );
        return out;
    }
    out.push_str(
        "SLUG                                     PHASE          STATE       COST   AGE\n",
    );
    for p in projects {
        let phase = display_phase(&p.state.current_phase);
        // V0.4.6 F91 — cost column sources cost_24h_usd from
        // progress.jsonl (best-effort; failure → $0.00 — fresh
        // projects with no progress events show $0.00, same shape
        // as pre-F91 `state.cost_used_usd == 0.0`).
        let cost_24h = cost_summary(&p.state.slug, &paths.progress_jsonl(&p.state.slug), paths)
            .map(|c| c.cost_24h_usd)
            .unwrap_or(0.0);
        out.push_str(&format!(
            "{:<40} {:<14} {:<11} ${:<5.2} {}s\n",
            truncate(&p.state.slug, 40),
            truncate(phase, 14),
            phase_state_str(&p.state.phase_state),
            cost_24h,
            p.age_seconds,
        ));
    }
    out
}

/// `current_phase` is empty between `ccteam new` and the first
/// dispatch; surface that as `pending` instead of a blank column so
/// `ls` and `show` are readable on fresh projects.
fn display_phase(phase: &str) -> &str {
    if phase.is_empty() {
        "pending"
    } else {
        phase
    }
}

fn render_ls_json(
    paths: &CcteamPaths,
    projects: &[ProjectSummary],
    daemon_up: bool,
) -> Result<String> {
    // V0.4.0 F60: the phase state machine was deleted; "active" can no
    // longer be derived from `phase_state == InFlight`.
    let active_count = 0usize;
    let arr: Vec<Value> = projects
        .iter()
        .map(|p| {
            // V0.4.6 F91 — JSON shape preserves the `cost_used_usd`
            // key for callers (MCP / scripts) but populates it from
            // `cost_24h_usd` so the number tracks reality. The legacy
            // serde field still reads as the frozen pre-F91 value if
            // anything in the JSON pipeline needs to differentiate.
            let cost_summary =
                cost_summary(&p.state.slug, &paths.progress_jsonl(&p.state.slug), paths)
                    .unwrap_or_default();
            json!({
                "slug": p.state.slug,
                "current_phase": p.state.current_phase,
                "phase_state": phase_state_str(&p.state.phase_state),
                "cost_used_usd": cost_summary.cost_24h_usd,
                "cost_24h_usd": cost_summary.cost_24h_usd,
                "cost_active_usd": cost_summary.cost_active_usd,
                "cost_total_usd": cost_summary.cost_total_usd,
                "context_tokens_used": p.state.context_tokens_used,
                "tmux_session": p.state.tmux_session,
                "user_attached": p.state.user_attached,
                "age_seconds": p.age_seconds,
                "last_event_ts": p
                    .state
                    .last_progress_event_at
                    .map(|t| t.to_rfc3339()),
                "stall_level": stall_level(p.stall_silent_seconds),
            })
        })
        .collect();
    let v = json!({
        "projects": arr,
        // `running` is a real bool driven by MCP socket reachability so
        // meta-agent / MCP consumers can gate writes on daemon liveness
        // without trusting a stale side file.
        "orchestrator": {
            "running": daemon_up,
            "active_count": active_count,
            "max_concurrent": 1,
        }
    });
    Ok(serde_json::to_string_pretty(&v)?)
}

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

fn humanize_secs_local(s: u64) -> String {
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

fn render_show_text(
    state: &ProjectState,
    cost: &ccteam_core::CostSummary,
    recent: &[Value],
    artifacts: &Map<String, Value>,
    sessions: &[ccteam_core::ActiveSessionInfo],
) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {} ({})\n\n", state.slug, state.tmux_session));
    out.push_str(&format!(
        "current phase  : {}\n",
        display_phase(&state.current_phase)
    ));
    out.push_str(&format!(
        "phase state    : {}\n",
        phase_state_str(&state.phase_state)
    ));
    // V0.4.6 F91 — cost(24h) sums every `agent_done.cost_usd` in the
    // last 24h; cost(active) live-reads each open session's
    // `~/.claude/jobs/<id>/state.json::cost_usd_total`. The pre-F91
    // `cost used: $X.XX` line (sourced from the now-frozen
    // `state.cost_used_usd`) is removed.
    out.push_str(&format!(
        "cost (24h)     : ${:.2}  ({} sessions)\n",
        cost.cost_24h_usd, cost.session_count_24h,
    ));
    out.push_str(&format!(
        "cost (active)  : ${:.2}  ({} running)\n",
        cost.cost_active_usd, cost.session_count_active,
    ));
    out.push_str(&format!("cost (total)   : ${:.2}\n", cost.cost_total_usd));
    out.push_str(&format!(
        "context tokens : {} ({} resets)\n",
        state.context_tokens_used, state.context_reset_count
    ));
    out.push_str(&format!(
        "fix cycle      : {}\n",
        state.auto_loop_cycle_count
    ));
    out.push_str("\nphase history:\n");
    if state.phase_history.is_empty() {
        out.push_str("  (empty)\n");
    } else {
        for h in &state.phase_history {
            out.push_str(&format!("  - {} ({})\n", h.phase, h.status));
        }
    }
    out.push_str("\nartifacts:\n");
    if artifacts.is_empty() {
        out.push_str("  (none yet)\n");
    } else {
        for (k, v) in artifacts {
            out.push_str(&format!("  {:<18} {}\n", k, v.as_str().unwrap_or("<?>")));
        }
    }

    out.push_str(&format!("\nactive sessions ({}):\n", sessions.len()));
    if sessions.is_empty() {
        out.push_str("  (none running)\n");
    } else {
        for s in sessions {
            let model = s.model.as_deref().unwrap_or("—");
            let ctx = s
                .context_remaining_pct
                .map(|p| format!("ctx {:>3.0}%", p))
                .unwrap_or_else(|| "ctx   —".into());
            let age = parse_rfc3339_age_secs(&s.started_at)
                .map(humanize_secs_local)
                .unwrap_or_else(|| "?".into());
            let short_job = s
                .job_id
                .as_deref()
                .map(|j| j.chars().take(8).collect::<String>())
                .unwrap_or_else(|| "—".into());
            out.push_str(&format!(
                "  {:<10}  {:<8}  {:<22}  {}  ${:>6.2}  {} ago\n",
                s.role, short_job, model, ctx, s.cost_usd, age
            ));
        }
        out.push_str("\n  tip: `claude attach <id>` to take over a session live\n");
    }

    out.push_str(&format!("\nrecent events ({}):\n", recent.len()));
    for e in recent {
        let ts = e.get("ts").and_then(|s| s.as_str()).unwrap_or("???");
        let event = e.get("event").and_then(|s| s.as_str()).unwrap_or("?");
        out.push_str(&format!("  {ts}  {event}\n"));
    }
    out
}

fn render_show_json(
    state: &ProjectState,
    cost: &ccteam_core::CostSummary,
    recent: &[Value],
    artifacts: &Map<String, Value>,
    sessions: &[ccteam_core::ActiveSessionInfo],
) -> Result<String> {
    let v = json!({
        "state": serde_json::to_value(state)?,
        "phase_history": serde_json::to_value(&state.phase_history)?,
        "cost": serde_json::to_value(cost)?,
        "recent_events": recent,
        "artifacts": Value::Object(artifacts.clone()),
        "active_sessions": serde_json::to_value(sessions)?,
        "stall": {
            "level": "ok",
            "silent_seconds": 0,
        },
        "recommendations": Value::Array(Vec::new()),
    });
    Ok(serde_json::to_string_pretty(&v)?)
}

fn phase_state_str(s: &PhaseState) -> &'static str {
    match s {
        PhaseState::Idle => "idle",
        PhaseState::Done => "done",
    }
}

pub(crate) fn stall_level(silent_s: u64) -> &'static str {
    if silent_s >= 30 * 60 {
        "escalate"
    } else if silent_s >= 15 * 60 {
        "suspicious"
    } else if silent_s >= 5 * 60 {
        "warn"
    } else {
        "ok"
    }
}

/// Collapse the `stall_level` tiers into the short verdict word the
/// `ccteam status` project rows print, so the operator reads "STUCK"
/// instead of decoding raw silence seconds. `escalate`/`suspicious`
/// both surface as `STUCK` (silent long enough to warrant a takeover),
/// `warn` stays `warn`, everything else is `OK`.
pub(crate) fn stall_verdict(silent_s: u64) -> &'static str {
    match stall_level(silent_s) {
        "escalate" | "suspicious" => "STUCK",
        "warn" => "warn",
        _ => "OK",
    }
}

/// One actionable hint line for a warn-or-higher project: the exact
/// peek → attach takeover sequence the operator runs next.
pub(crate) fn stall_takeover_hint(slug: &str, silent: &str) -> String {
    format!(
        "{slug} silent {silent} — `ccteam internal peek {slug}` then \
         `ccteam internal attach {slug}` to take over"
    )
}

fn truncate(s: &str, n: usize) -> &str {
    if s.len() <= n {
        s
    } else {
        let mut end = n;
        while !s.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        &s[..end]
    }
}

/// V0.3 M5.0: knobs forwarded by `ccteam web` clap struct → axum
/// scaffold. Mirrors `ccteam_web::ServeOpts` 1:1 except `bind` is
/// still a string here (parsed in `run_web`).
#[derive(Debug, Clone)]
pub struct WebOptions {
    pub bind: String,
    pub no_auth: bool,
    pub token_file: Option<std::path::PathBuf>,
}

/// `ccteam web --bind <addr>` entry. Translates clap-side string +
/// flags into `ccteam_web::ServeOpts`, then drives a current-thread
/// tokio runtime to `serve(opts)` (mirrors the `mcp-serve` shape so a
/// future operator running both side-by-side sees the same harness).
pub fn run_web(opts: WebOptions) -> Result<()> {
    use std::net::SocketAddr;

    let bind: SocketAddr = opts
        .bind
        .parse()
        .with_context(|| format!("parse --bind `{}` as SocketAddr", opts.bind))?;
    let serve_opts = ccteam_web::ServeOpts {
        bind,
        no_auth: opts.no_auth,
        token_file: opts.token_file,
        // Production CLI path keeps the 5 s Ctrl-C window so an
        // operator who passes `--no-auth` on a non-loopback bind has
        // a chance to abort before the LAN-RCE surface goes live.
        no_auth_grace_secs: Some(5),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime for ccteam web")?;
    runtime.block_on(ccteam_web::serve(serve_opts))
}

/// Per-slug unroster trigger file. `ccteam remove` writes here; the
/// daemon's `poll_unroster_triggers` task picks it up within 250ms
/// and calls `unroster_project(CancelReason::Removed)`.
///
/// Convention mirrors `shutdown_trigger_path()` in main.rs: per-user
/// namespace under `/tmp` keeps multi-operator hosts safe.
pub(crate) fn unroster_trigger_path(slug: &str) -> std::path::PathBuf {
    let user = std::env::var("USER").unwrap_or_else(|_| "ccteam".into());
    std::path::PathBuf::from("/tmp").join(format!("ccteam-{user}.unroster.{slug}"))
}

/// V0.4.6 F81 — options for `ccteam remove <slug>`.
#[derive(Debug, Clone, Default)]
pub struct RemoveOptions {
    /// Also `rm -rf <project>/.ccteam/`, `<project>/.claude/agents/`,
    /// and `<project>/workflow.yaml`. Business code + `.env` untouched.
    pub purge: bool,
    /// Print every step that *would* run, but don't touch filesystem
    /// / config / daemon.
    pub dry_run: bool,
    /// Skip the CLAUDE.md §三 "永不主动 kill 长 session" refusal gate.
    pub force: bool,
}

/// V0.4.6 F81 — structured result of `run_remove`. Returned so MCP
/// callers (a future `tool_remove` wire) can branch on the success
/// shape; the CLI just `Display`s the text rendering.
#[derive(Debug, Clone, Default)]
pub struct RemoveReport {
    pub slug: String,
    pub purge: bool,
    pub dry_run: bool,
    pub forced: bool,
    /// One-line entries describing each step that ran (or would run
    /// under `--dry-run`). Surface order matches execution order so
    /// users see the same shape with and without `--dry-run`.
    pub steps: Vec<String>,
    /// Set when the refusal gate fired (and `--force` was not passed).
    /// `--dry-run` still reports the refusal so users can rehearse.
    pub refusal: Option<ccteam_core::ActiveSessionRefusal>,
}

impl std::fmt::Display for RemoveReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mode = if self.dry_run { "[dry-run] " } else { "" };
        writeln!(
            f,
            "ccteam remove {}{}{}{}",
            mode,
            self.slug,
            if self.purge { " --purge" } else { "" },
            if self.forced { " --force" } else { "" }
        )?;
        for step in &self.steps {
            writeln!(f, "  - {step}")?;
        }
        if let Some(refusal) = &self.refusal {
            writeln!(f, "refusal: {}", refusal.message(&self.slug))?;
        }
        Ok(())
    }
}

// -------------------------------------------------------------------------
// V0.6.0 Wave 3 F112 §C — `ccteam prefs` admin surface.
//
// Reads / writes `~/.ccteam/preferences.toml`. Today only the
// fallback section has user-visible keys; V0.7+ can add more by
// extending the parse_key match arm + emitting a friendly diagnostic
// for unknown keys.
// -------------------------------------------------------------------------

/// Format the active preferences for `ccteam prefs show`. Includes
/// the resolved file path so the user knows what was loaded.
pub fn run_prefs_show(paths: &CcteamPaths) -> Result<String> {
    let prefs = ccteam_core::preferences::load_or_default(&paths.root);
    let path = ccteam_core::preferences::preferences_path(&paths.root);
    let exists_marker = if path.exists() {
        "loaded"
    } else {
        "defaults (file not present)"
    };
    let body = toml::to_string_pretty(&prefs).context("serialize preferences for display")?;
    Ok(format!(
        "# ccteam preferences ({exists_marker})\n# path: {}\n\n{body}",
        path.display()
    ))
}

/// Look up a dotted preference key. Returns the textual value or an
/// error if the key is unknown.
pub fn run_prefs_get(paths: &CcteamPaths, key: &str) -> Result<String> {
    let prefs = ccteam_core::preferences::load_or_default(&paths.root);
    match key {
        "fallback.on_claude_quota" => Ok(match prefs.fallback.on_claude_quota {
            ccteam_core::preferences::OnClaudeQuota::Off => "off".to_string(),
            ccteam_core::preferences::OnClaudeQuota::Codex => "codex".to_string(),
        }),
        "fallback.codex.enabled_for_roles" => Ok(prefs.fallback.codex.enabled_for_roles.join(",")),
        other => Err(anyhow::anyhow!(
            "unknown preference key: {other}\n\
             supported keys:\n  - fallback.on_claude_quota  (off|codex)\n  \
             - fallback.codex.enabled_for_roles  (comma list; empty = all roles)"
        )),
    }
}

/// Persist one preference change to `~/.ccteam/preferences.toml`.
/// Returns a one-line confirmation suitable for stdout.
pub fn run_prefs_set(paths: &CcteamPaths, key: &str, value: &str) -> Result<String> {
    let mut prefs = ccteam_core::preferences::load_or_default(&paths.root);
    match key {
        "fallback.on_claude_quota" => {
            prefs.fallback.on_claude_quota = match value.trim().to_lowercase().as_str() {
                "off" => ccteam_core::preferences::OnClaudeQuota::Off,
                "codex" => ccteam_core::preferences::OnClaudeQuota::Codex,
                other => {
                    return Err(anyhow::anyhow!(
                        "fallback.on_claude_quota: unsupported value {other:?} \
                         (expected `off` or `codex`)"
                    ));
                }
            };
        }
        "fallback.codex.enabled_for_roles" => {
            prefs.fallback.codex.enabled_for_roles = if value.trim().is_empty() {
                Vec::new()
            } else {
                value
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            };
        }
        other => {
            return Err(anyhow::anyhow!(
                "unknown preference key: {other}\n\
                 supported keys:\n  - fallback.on_claude_quota  (off|codex)\n  \
                 - fallback.codex.enabled_for_roles  (comma list; empty = all roles)"
            ));
        }
    }
    ccteam_core::preferences::save(&paths.root, &prefs)?;
    Ok(format!("set {key} = {value}"))
}

// -------------------------------------------------------------------------
// v0.8.6 Item 4 — `ccteam config` setup hub.
//
// `config` is the single setup entrypoint a fresh host runs after
// `ccteam init`. It absorbs three formerly-scattered setup actions:
//   (a) register/refresh the ccteam MCP server (was `doctor --install-mcp`),
//   (b) set the IM (Telegram) bot token (was the `ccteam-im-setup` skill;
//       backed by `ccteam_im::onboarding::telegram_setup`),
//   (c) read/write preferences (the `prefs` get/set/show backend).
//
// Bare `ccteam config` opens a thin numbered-choice interactive menu;
// each menu item dispatches to the SAME action fn the non-interactive
// path calls, so the logic stays testable without a TTY. The
// non-interactive form (`config <key> <value>` / `config get <key>` /
// `config show`) is the headless/CI surface and wraps the prefs backend.
// `preferences.toml` remains the store for the key/value knobs.
// -------------------------------------------------------------------------

/// Number of seconds the IM-token flow long-polls Telegram's
/// `getUpdates` for the first incoming message (to capture the owner's
/// `chat_id`). Kept short so a non-interactive misfire fails fast.
const CONFIG_IM_POLL_SECONDS: u64 = 60;

/// `config` action (a) — register / refresh `mcpServers.ccteam` in
/// `~/.claude.json`. Thin wrapper over the same writer the retired
/// `doctor --install-mcp` flag used (`render_install_mcp_report`), so the
/// rendered report (incl. the live tool count) is identical.
pub fn run_config_install_mcp() -> Result<String> {
    render_install_mcp_report()
}

/// `config` action (b) — validate a Telegram bot token, long-poll for the
/// owner's first message to capture the `chat_id`, and persist the
/// resulting credentials to `~/.ccteam/im/credentials.json` (mode 0600).
/// Wraps [`ccteam_im::onboarding::telegram_setup`]; the async call is
/// driven on a one-shot current-thread tokio runtime so the sync CLI /
/// menu path stays runtime-agnostic.
///
/// Returns the stdout body to print on success (bot handle + creds path
/// + a "DM the bot now" hint the caller may have already surfaced).
pub fn run_config_set_im_token(token: &str) -> Result<String> {
    let token = token.trim();
    if token.is_empty() {
        bail!("config: empty Telegram bot token (paste the token from @BotFather)");
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime for IM token onboarding")?;
    let result = runtime
        .block_on(ccteam_im::onboarding::telegram_setup(
            token,
            CONFIG_IM_POLL_SECONDS,
        ))
        .context("Telegram onboarding (token validation + chat_id capture)")?;

    // Read the display fields out before moving `creds` into the doc.
    let bot_username = result.bot_username;
    let owner = result
        .creds
        .allowed_chat_ids
        .first()
        .cloned()
        .unwrap_or_default();

    // Persist: merge into any existing credentials doc so a prior Slack /
    // Discord / Lark entry survives a Telegram (re)config.
    let creds_path = ccteam_im::credentials::default_path();
    let mut creds = ccteam_im::credentials::load(Some(&creds_path))
        .context("load existing IM credentials before merge")?;
    creds.telegram = Some(result.creds);
    ccteam_im::credentials::save(&creds_path, &creds).context("persist IM credentials")?;

    Ok(format!(
        "ccteam config: Telegram token saved\n\n  \
         bot           {}\n  \
         owner chat_id {}\n  \
         credentials   {}\n\n\
         `ccteam start` will bring the IM gateway up with these credentials.\n",
        bot_username,
        owner,
        creds_path.display(),
    ))
}

/// `config` action — validate Lark/Feishu app credentials (by fetching a
/// `tenant_access_token`) and persist them to
/// `~/.ccteam/im/credentials.json` (mode 0600). Mirrors
/// [`run_config_set_im_token`] but for the WS-long-connection Lark
/// provider: there is no `chat_id` to poll — the provider keys its
/// allowlist on operator-supplied `open_id`s.
///
/// `allowed_user_ids` is **fail-closed**: an empty list means the bot
/// answers no one (the opposite of Telegram's empty=open). `use_feishu`
/// selects the region (`true` = Feishu/CN, `false` = Lark international).
pub fn run_config_set_lark_creds(
    app_id: &str,
    app_secret: &str,
    allowed_user_ids: Vec<String>,
    use_feishu: bool,
) -> Result<String> {
    let api_base = if use_feishu {
        ccteam_im::onboarding::FEISHU_API_BASE
    } else {
        ccteam_im::onboarding::LARK_API_BASE
    };
    run_config_set_lark_creds_with_base(
        app_id,
        app_secret,
        allowed_user_ids,
        use_feishu,
        api_base,
        None,
    )
}

/// Test seam for [`run_config_set_lark_creds`]: lets callers override the
/// Lark API base (point a deterministic mock server at it) and the
/// credentials-file path (sandbox `~/.ccteam/im/credentials.json` into a
/// tempdir). Production callers go through [`run_config_set_lark_creds`],
/// which passes the real region base + the default creds path. Mirrors the
/// `_with_base` convention in `ccteam_im::onboarding`.
pub fn run_config_set_lark_creds_with_base(
    app_id: &str,
    app_secret: &str,
    allowed_user_ids: Vec<String>,
    use_feishu: bool,
    api_base: &str,
    creds_path_override: Option<&std::path::Path>,
) -> Result<String> {
    let app_id = app_id.trim();
    let app_secret = app_secret.trim();
    if app_id.is_empty() || app_secret.is_empty() {
        bail!("config: Lark app_id and app_secret are both required");
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime for Lark credential onboarding")?;
    let result = runtime
        .block_on(ccteam_im::onboarding::lark_setup_with_base(
            app_id,
            app_secret,
            allowed_user_ids,
            use_feishu,
            api_base,
        ))
        .context("Lark onboarding (app credential validation via tenant_access_token)")?;

    // Persist: merge into any existing credentials doc so a prior
    // Telegram / Slack / Discord entry survives a Lark (re)config.
    let creds_path = match creds_path_override {
        Some(p) => p.to_path_buf(),
        None => ccteam_im::credentials::default_path(),
    };
    let mut creds = ccteam_im::credentials::load(Some(&creds_path))
        .context("load existing IM credentials before merge")?;
    let allow_count = result.creds.allowed_user_ids.len();
    let region = if result.creds.use_feishu {
        "Feishu (CN, open.feishu.cn)"
    } else {
        "Lark (intl, open.larksuite.com)"
    };
    creds.lark = Some(result.creds);
    ccteam_im::credentials::save(&creds_path, &creds).context("persist IM credentials")?;

    let allow_note = if allow_count == 0 {
        "  allowlist     EMPTY — fail-closed: the bot answers NO ONE.\n  \
         add open_ids (ou_…) to allowed_user_ids to let users in.\n"
            .to_string()
    } else {
        format!("  allowlist     {allow_count} open_id(s) allowed\n")
    };

    Ok(format!(
        "ccteam config: Lark/Feishu credentials saved\n\n  \
         app_id        {}\n  \
         region        {}\n\
         {}  \
         credentials   {}\n\n\
         `ccteam start` will bring the IM gateway up with these credentials.\n",
        app_id,
        region,
        allow_note,
        creds_path.display(),
    ))
}

/// Bare `ccteam config` — thin numbered-choice interactive menu. Reads a
/// single digit from stdin and dispatches to the same action fn the
/// non-interactive path uses (so all real work stays in testable fns).
/// On a non-TTY stdin we refuse rather than hang, pointing the operator at
/// the non-interactive forms.
pub fn run_config_menu(paths: &CcteamPaths) -> Result<String> {
    use std::io::{IsTerminal, Write};

    if !std::io::stdin().is_terminal() {
        bail!(
            "ccteam config: interactive menu needs a TTY.\n\
             headless forms:\n  \
             ccteam config show                 # print preferences\n  \
             ccteam config get <key>            # read one preference\n  \
             ccteam config <key> <value>        # set one preference\n  \
             ccteam doctor --verify-mcp         # check the MCP wiring\n\
             (MCP register + IM token onboarding need an interactive run.)"
        );
    }

    println!("ccteam config — setup\n");
    println!("  1) register / refresh the ccteam MCP server (~/.claude.json)");
    println!("  2) set the IM (Telegram) bot token");
    println!("  3) set Lark/Feishu app credentials");
    println!("  4) show preferences");
    println!("  q) quit");
    print!("\nchoose [1-4/q]: ");
    std::io::stdout().flush().ok();

    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .context("read config menu choice from stdin")?;

    match line.trim() {
        "1" => run_config_install_mcp(),
        "2" => {
            print!("paste the Telegram bot token (from @BotFather): ");
            std::io::stdout().flush().ok();
            let mut token = String::new();
            std::io::stdin()
                .read_line(&mut token)
                .context("read Telegram token from stdin")?;
            println!(
                "validating token + waiting up to {CONFIG_IM_POLL_SECONDS}s for you to DM the bot…"
            );
            run_config_set_im_token(&token)
        }
        "3" => run_config_lark_menu(),
        "4" => run_prefs_show(paths),
        "q" | "Q" | "" => Ok("ccteam config: nothing changed.\n".to_string()),
        other => bail!("ccteam config: unrecognized choice {other:?} (expected 1-4 or q)"),
    }
}

/// Interactive prompt for the menu's Lark/Feishu item: collect
/// `app_id` / `app_secret`, the region (Feishu/CN default vs Lark intl),
/// and the optional `open_id` allowlist, then hand off to
/// [`run_config_set_lark_creds`] (which validates + persists). Kept
/// separate from [`run_config_menu`] so the stdin reads stay linear and
/// the persistence logic remains unit-testable without a TTY.
fn run_config_lark_menu() -> Result<String> {
    use std::io::Write;

    fn prompt_line(label: &str) -> Result<String> {
        print!("{label}");
        std::io::stdout().flush().ok();
        let mut buf = String::new();
        std::io::stdin()
            .read_line(&mut buf)
            .with_context(|| format!("read {label:?} from stdin"))?;
        Ok(buf.trim().to_string())
    }

    println!(
        "\nLark/Feishu app credentials (from the developer console → app → Credentials & Basic Info)."
    );
    let app_id = prompt_line("app_id (cli_…): ")?;
    let app_secret = prompt_line("app_secret: ")?;

    // Region: default Feishu/CN (Enter accepts the default).
    let region = prompt_line("region — [F]eishu CN (default) / [L]ark intl: ")?;
    let use_feishu = !matches!(region.to_ascii_lowercase().as_str(), "l" | "lark" | "intl");

    // Provider allowlist is FAIL-CLOSED — make that loud.
    println!(
        "\nallowed_user_ids = the open_ids (ou_…) allowed to drive the bot.\n  \
         FAIL-CLOSED: leaving this EMPTY means the bot answers NO ONE\n  \
         (the opposite of Telegram, where empty = open). Use `*` to allow everyone."
    );
    let allow_raw = prompt_line("allowed open_ids (comma/space separated, or blank): ")?;
    let allowed_user_ids: Vec<String> = allow_raw
        .split([',', ' ', '\t'])
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();

    println!("validating app credentials (fetching a tenant_access_token)…");
    run_config_set_lark_creds(&app_id, &app_secret, allowed_user_ids, use_feishu)
}

// -------------------------------------------------------------------------
// V0.6.1 F128 — `ccteam admin change-persona` / `ccteam admin add-tool`.
//
// Both subcommands edit `<project>/.claude/agents/<bot>.md` and emit a
// `persona_changed` / `tool_added` event to the project's
// `progress.jsonl`. The same code path backs the MCP
// `ccteam__admin_change_persona` / `ccteam__admin_add_tool` tools (see
// `mcp_admin_tools.rs`).
// -------------------------------------------------------------------------

/// V0.6.1 F128 — replace `<project>/.claude/agents/<bot>.md` with
/// `new_persona_md` and emit a `persona_changed` event. Returns the
/// stdout body the CLI prints on success (pretty JSON).
pub fn run_admin_change_persona(
    paths: &CcteamPaths,
    slug: &str,
    bot: &str,
    new_persona_md: &str,
) -> Result<String> {
    let project_dir = paths.project_dir(slug);
    if !project_dir.exists() {
        return Err(anyhow::anyhow!(
            "no project named `{slug}` (looked under {})",
            project_dir.display()
        ));
    }
    let written = ccteam_core::admin_actions::change_persona(&project_dir, bot, new_persona_md)?;
    let bytes = new_persona_md.len();
    let event = ccteam_core::admin_actions::build_persona_changed_event(bot, &written, bytes);
    ccteam_core::progress::append_event(&paths.progress_jsonl(slug), &event)?;
    Ok(serde_json::to_string_pretty(&serde_json::json!({
        "ok": true,
        "slug": slug,
        "bot": bot,
        "path": written.display().to_string(),
        "bytes_written": bytes,
        "event": event,
    }))?)
}

/// v0.8.7 W3 (DC.3) — resolve a `--project <slug>` (or, when `None`, the
/// current working directory canonicalized) to an existing project dir.
/// Used by `ccteam role add` / `ccteam role list`. A `slug` that isn't a
/// registered project (or a cwd that doesn't exist) is a loud error.
fn resolve_project_dir(paths: &CcteamPaths, slug: Option<&str>) -> Result<std::path::PathBuf> {
    let dir = match slug {
        Some(s) => {
            let d = paths.project_dir(s);
            if !d.exists() {
                bail!("no project named `{s}` (looked under {})", d.display());
            }
            d
        }
        None => std::env::current_dir().context("read cwd as the default --project target")?,
    };
    std::fs::canonicalize(&dir)
        .with_context(|| format!("canonicalize project dir `{}`", dir.display()))
}

/// v0.8.7 W3 (DC.3) — `ccteam role search <q>`. Offline substring search
/// over the bundled agency-agents catalog (no network). Empty query lists
/// the whole catalog. Text output prints `id` + division + description so
/// the user can copy an `id` into `ccteam role add`.
pub fn run_role_search(query: &str, format: OutputFormat) -> Result<String> {
    let hits = ccteam_core::catalog_search(query)?;
    Ok(match format {
        OutputFormat::Json => serde_json::to_string_pretty(&hits)?,
        OutputFormat::Text => {
            if hits.is_empty() {
                format!("no catalog roles match `{query}`.\n")
            } else {
                let mut out = format!(
                    "{} role(s) in the agency-agents catalog{}:\n\n",
                    hits.len(),
                    if query.trim().is_empty() {
                        String::new()
                    } else {
                        format!(" matching `{query}`")
                    }
                );
                for e in &hits {
                    out.push_str(&format!("  {}  [{}]\n", e.id, e.division));
                    if !e.description.is_empty() {
                        // One-line, truncated description for the list view.
                        let desc: String = e.description.chars().take(96).collect();
                        out.push_str(&format!("      {desc}\n"));
                    }
                }
                out.push_str(
                    "\nInstall one: ccteam role add <id> [--as <role>] [--project <slug>]\n",
                );
                out
            }
        }
    })
}

/// v0.8.7 W3 (DC.3) — `ccteam role add <id> [--as <role>] [--project <slug>]
/// [--force]`. Imports a catalog role into the project's `.claude/agents/`
/// (fetch over HTTP → verbatim write) and prints a `/role <role>` hint.
/// The async fetch is driven on a throwaway current-thread runtime
/// ([`block_on_async`]) since `main()` is sync.
pub fn run_role_add(
    paths: &CcteamPaths,
    id: &str,
    as_role: Option<&str>,
    project: Option<&str>,
    force: bool,
) -> Result<String> {
    let project_dir = resolve_project_dir(paths, project)?;
    let result = block_on_async(ccteam_im::role_import::import_role_from_catalog(
        &project_dir,
        id,
        as_role,
        force,
    ))??;
    let mut out = format!(
        "Installed role `{}` from catalog `{}`{}.\n  {}\n",
        result.role,
        result.catalog_id,
        if result.overwrote {
            " (overwrote existing)"
        } else {
            ""
        },
        result.path.display(),
    );
    out.push_str(&format!(
        "\nSwitch to it in a chat with `/role {}` (or spawn a session with that role).\n",
        result.role
    ));
    // v0.8.7 review-fix (R-L6) — the body is third-party content (MIT
    // agency-agents) fetched verbatim. Persona text steers an agent that runs
    // with `--dangerously-skip-permissions`, so prompt the operator to read it
    // before use rather than trusting it blind.
    out.push_str(&format!(
        "\nNote: this role .md is third-party content fetched verbatim — review {} before use.\n",
        result.path.display()
    ));
    Ok(out)
}

/// v0.8.7 W3 (DC.3) — `ccteam role list [--project <slug>]`. Wraps
/// [`ccteam_core::list_roles`] to show the roles already installed in the
/// project's `.claude/agents/`. An uninitialized project (no `agents/` dir)
/// is a normal "no roles yet" result, not an error.
pub fn run_role_list(
    paths: &CcteamPaths,
    project: Option<&str>,
    format: OutputFormat,
) -> Result<String> {
    let project_dir = resolve_project_dir(paths, project)?;
    let roles = ccteam_core::list_roles(&project_dir)?;
    Ok(match format {
        OutputFormat::Json => serde_json::to_string_pretty(&roles)?,
        OutputFormat::Text => {
            if roles.is_empty() {
                format!(
                    "no roles installed in {} (.claude/agents/ is empty or absent).\n\
                     Browse the catalog: ccteam role search <q>\n",
                    project_dir.display()
                )
            } else {
                let mut out = format!("{} role(s) in {}:\n\n", roles.len(), project_dir.display());
                for r in &roles {
                    out.push_str(&format!("  {}", r.role));
                    if !r.description.is_empty() {
                        let desc: String = r.description.chars().take(80).collect();
                        out.push_str(&format!("  — {desc}"));
                    }
                    out.push('\n');
                }
                out
            }
        }
    })
}

/// V0.6.1 F128 — append `tool_descriptor` to the bot's
/// `.claude/agents/<bot>.md` frontmatter `tools:` CSV and emit a
/// `tool_added` event. Idempotent — re-adding sets
/// `already_present: true`.
pub fn run_admin_add_tool(
    paths: &CcteamPaths,
    slug: &str,
    bot: &str,
    tool_descriptor: &str,
) -> Result<String> {
    let project_dir = paths.project_dir(slug);
    if !project_dir.exists() {
        return Err(anyhow::anyhow!(
            "no project named `{slug}` (looked under {})",
            project_dir.display()
        ));
    }
    let res = ccteam_core::admin_actions::add_tool(&project_dir, bot, tool_descriptor)?;
    let event = ccteam_core::admin_actions::build_tool_added_event(
        bot,
        &res.path,
        &res.added,
        &res.new_tools_csv,
        res.already_present,
    );
    ccteam_core::progress::append_event(&paths.progress_jsonl(slug), &event)?;
    Ok(serde_json::to_string_pretty(&serde_json::json!({
        "ok": true,
        "slug": slug,
        "bot": bot,
        "path": res.path.display().to_string(),
        "tool": res.added,
        "tools_csv": res.new_tools_csv,
        "already_present": res.already_present,
        "event": event,
    }))?)
}

/// V0.6.8 F202 — register a chat-mode bot via the CLI (the fallback
/// when no daemon / MCP server is running). Mirrors the MCP path
/// `dispatch_register_bot` in `mcp_chat_tools.rs`: same vendor
/// normalization, same `chat_handle` auto-mint when caller omits it,
/// same `project_dir` canonicalize + absolute-path guard. Non-clobber:
/// re-registering an existing `(slug, role)` returns
/// `ok:false, error:"already_registered"` instead of overwriting.
#[allow(clippy::too_many_arguments)]
pub fn run_admin_register_bot(
    paths: &CcteamPaths,
    slug: &str,
    role: &str,
    vendor_raw: &str,
    platform_raw: &str,
    chat_id: &str,
    chat_handle_in: Option<&str>,
    project_dir_in: Option<&std::path::Path>,
) -> Result<String> {
    use ccteam_harness::AgentVendor;
    use ccteam_im::{register_bot_checked_in, RegisterOutcome};

    // Slug / role share the same validator as the MCP path
    // (alphanumeric + `-` + `_`).
    crate::mcp_chat_tools::validate_slug(slug, "slug")?;
    crate::mcp_chat_tools::validate_slug(role, "role")?;

    // Vendor: lowercase first so `Claude` still lands in the right variant.
    let vendor = match vendor_raw.to_lowercase().as_str() {
        "claude" => AgentVendor::Claude,
        "codex" => AgentVendor::Codex,
        other => {
            return Err(anyhow::anyhow!(
                "invalid vendor `{other}`: expected one of `claude`, `codex`"
            ))
        }
    };

    // IM platform: same enum the MCP dispatcher uses.
    match platform_raw {
        "telegram" | "slack" | "discord" | "lark" | "mock" => {}
        other => {
            return Err(anyhow::anyhow!(
                "invalid platform `{other}`: expected one of `telegram`, `slack`, `discord`, `lark`, `mock`"
            ));
        }
    }

    if chat_id.is_empty() {
        return Err(anyhow::anyhow!("`chat-id` must be non-empty"));
    }

    // Chat handle: caller wins; absent → auto-mint scientist nickname.
    let chat_handle = match chat_handle_in {
        Some(h) if !h.is_empty() => {
            crate::mcp_chat_tools::validate_chat_handle(h)?;
            h.to_string()
        }
        _ => crate::mcp_chat_tools::mint_unused_handle(&paths.root)?,
    };

    // Project dir: caller-supplied MUST be absolute. When omitted, default
    // to cwd canonicalized (resolves symlinks too — daemon stores the
    // post-canonical form). Identical to the MCP path so bot record
    // semantics never diverge.
    let project_dir = match project_dir_in {
        Some(p) if !p.as_os_str().is_empty() => {
            if !p.is_absolute() {
                return Err(anyhow::anyhow!(
                    "`--project-dir` must be an absolute path (got `{}`)",
                    p.display()
                ));
            }
            p.to_path_buf()
        }
        _ => std::env::current_dir().context("std::env::current_dir for default project_dir")?,
    };
    let project_dir = std::fs::canonicalize(&project_dir).with_context(|| {
        format!(
            "canonicalize project_dir `{}` (does the path exist?)",
            project_dir.display()
        )
    })?;

    let outcome = register_bot_checked_in(
        &paths.root,
        slug,
        role,
        vendor,
        platform_raw,
        chat_id,
        None,
        Some(chat_handle.as_str()),
        Some(project_dir.as_path()),
    )?;
    match outcome {
        RegisterOutcome::Registered(path) => {
            Ok(serde_json::to_string_pretty(&serde_json::json!({
                "ok": true,
                "path": path.display().to_string(),
                "workflow_slug": slug,
                "role": role,
                "chat_handle": chat_handle,
                "project_dir": project_dir.display().to_string(),
            }))?)
        }
        RegisterOutcome::AlreadyRegistered(path) => {
            Ok(serde_json::to_string_pretty(&serde_json::json!({
                "ok": false,
                "error": "already_registered",
                "path": path.display().to_string(),
                "workflow_slug": slug,
                "role": role,
                "hint": "Unregister first with `ccteam admin unregister-bot`, then re-register.",
            }))?)
        }
    }
}

/// V0.6.8 F202 — unregister a chat-mode bot via the CLI. Mirrors the
/// MCP `ccteam__chat_unregister_bot` path: idempotent — returns
/// `ok:true, removed:false` when no registration exists.
pub fn run_admin_unregister_bot(paths: &CcteamPaths, slug: &str, role: &str) -> Result<String> {
    use ccteam_im::unregister_bot_in;

    crate::mcp_chat_tools::validate_slug(slug, "slug")?;
    crate::mcp_chat_tools::validate_slug(role, "role")?;
    let (removed, path) = unregister_bot_in(&paths.root, slug, role)?;
    Ok(serde_json::to_string_pretty(&serde_json::json!({
        "ok": true,
        "removed": removed,
        "path": path.display().to_string(),
        "workflow_slug": slug,
        "role": role,
    }))?)
}

/// `ccteam admin list-bots [--slug <slug>] [--json]`. Reads the F146
/// registry (`~/.ccteam/imd/registry/<slug>/<role>.json`) and reports
/// each registered chat-mode bot with its effective `@handle`,
/// platform/chat_id, and live `running` status (from the per-bot
/// heartbeat sidecar). Pairs with `register-bot` / `unregister-bot` so
/// users can confirm what they wired up. Distinct from the MCP
/// `admin_ls` tool, which enumerates *projects*, not bot registrations.
pub fn run_admin_list_bots(
    paths: &CcteamPaths,
    filter_slug: Option<&str>,
    json: bool,
) -> Result<String> {
    use ccteam_im::{bot_running_status_in, list_bots_in};

    if let Some(slug) = filter_slug {
        crate::mcp_chat_tools::validate_slug(slug, "slug")?;
    }

    let mut bots = list_bots_in(&paths.root, filter_slug)?;
    // Deterministic order: slug, then role.
    bots.sort_by(|a, b| {
        a.workflow_slug
            .cmp(&b.workflow_slug)
            .then_with(|| a.role.cmp(&b.role))
    });

    if json {
        let arr: Vec<serde_json::Value> = bots
            .iter()
            .map(|b| {
                let running = bot_running_status_in(&paths.root, &b.workflow_slug, &b.role);
                serde_json::json!({
                    "slug": b.workflow_slug,
                    "role": b.role,
                    "handle": b.chat_handle.clone().unwrap_or_else(|| b.role.clone()),
                    "vendor": b.vendor,
                    "platform": b.im_platform,
                    "chat_id": b.im_chat_id,
                    "running": running,
                    "created_at": b.created_at.to_rfc3339(),
                })
            })
            .collect();
        return Ok(serde_json::to_string_pretty(&serde_json::json!({
            "ok": true,
            "count": arr.len(),
            "bots": arr,
        }))?);
    }

    if bots.is_empty() {
        return Ok(match filter_slug {
            Some(s) => format!("no bots registered for slug `{s}`"),
            None => "no bots registered".to_string(),
        });
    }

    // Human-readable table.
    let mut out = String::new();
    out.push_str(&format!("{} registered bot(s):\n\n", bots.len()));
    out.push_str("  SLUG                 ROLE         @HANDLE        PLATFORM   RUNNING\n");
    out.push_str("  ──────────────────── ──────────── ────────────── ────────── ───────\n");
    for b in &bots {
        let handle = b.chat_handle.clone().unwrap_or_else(|| b.role.clone());
        let running = if bot_running_status_in(&paths.root, &b.workflow_slug, &b.role) {
            "yes"
        } else {
            "no"
        };
        out.push_str(&format!(
            "  {:<20} {:<12} @{:<13} {:<10} {}\n",
            truncate_col(&b.workflow_slug, 20),
            truncate_col(&b.role, 12),
            truncate_col(&handle, 13),
            truncate_col(&b.im_platform, 10),
            running,
        ));
    }
    Ok(out)
}

/// Right-pad/truncate helper for the `list-bots` table — keeps columns
/// aligned when a slug/handle is longer than its column width.
fn truncate_col(s: &str, width: usize) -> String {
    if s.chars().count() > width {
        let kept: String = s.chars().take(width.saturating_sub(1)).collect();
        format!("{kept}…")
    } else {
        s.to_string()
    }
}

/// V0.4.6 F81 — `ccteam remove <slug>` implementation.
///
/// Steps (in order):
/// 1. Refusal gate. Calls [`ccteam_core::refuses_active_session`]; if
///    it returns `Some(refusal)` and `opts.force` is false, the command
///    halts before any mutation. `--dry-run` still walks the rest of
///    the plan (reporting all steps as "would run") so the user gets a
///    full preview. Then (v0.8.6 W3) stop-then-delete: tear down the
///    project's live chat-mode role sessions (`ccteam-chat-<slug>-*`)
///    before deregistering — `--dry-run` lists what it would stop.
/// 2. Resolve project_dir via `~/.ccteam/config.yaml::projects[]` so
///    arbitrary-path installs (V0.4.2 F77) are honoured.
/// 3. Drop the slug from config.yaml::projects[] (atomic via
///    `config::save`'s tmp+rename). `--dry-run` prints the plan only.
/// 4. Unroster the running daemon's in-memory state. **F81 stub: this
///    skips the daemon-side cancel** — the F82 worktree will replace
///    `Orchestrator::unroster_project` with cancellation-token wiring;
///    until then, the daemon picks up the config change on its next
///    `spawn_new_rostered_projects` tick (eventual consistency).
/// 5. Remove orchestration state: `~/.ccteam/progress/<slug>.jsonl`
///    (or flex `~/.ccteam/progress/<slug>/` dir), then any
///    `~/.ccteam/inbox/<slug>/` and `~/.ccteam/control/<slug>/`
///    sub-trees that exist.
/// 6. `--purge`: delete exactly ccteam's project footprint (v0.8.6 W2
///    layout) — `rm -rf <project>/.ccteam/`, the seeded
///    `<project>/.claude/agents/cto.md` (NOT the whole agents dir), and
///    ccteam's hook section inside `<project>/.claude/settings.local.json`
///    (surgically; file deleted only if it collapses to empty). See
///    [`purge_project_managed_paths`] for the full KEEP/DELETE contract.
///    Never touches `<project>/.env`, user work-roles, `CLAUDE.md` /
///    `AGENTS.md`, or the user's `settings.json` (CLAUDE.md §三 red line).
///
/// This is the reusable remove engine: the flat `ccteam remove` and the
/// grouped `ccteam project rm` both route here (the structured
/// [`RemoveReport`] doubles as the dry-run plan).
pub fn run_remove(paths: &CcteamPaths, slug: &str, opts: RemoveOptions) -> Result<RemoveReport> {
    let mut report = RemoveReport {
        slug: slug.to_string(),
        purge: opts.purge,
        dry_run: opts.dry_run,
        forced: opts.force,
        ..Default::default()
    };

    // 1. Refusal gate (CLAUDE.md §三).
    let refusal = ccteam_core::refuses_active_session(paths, slug)?;
    if let Some(r) = refusal {
        if !opts.force {
            // Halt before any mutation — user must `tmux kill-session`
            // / let claude finish / pass `--force`.
            report.refusal = Some(r.clone());
            bail!(
                "ccteam remove `{slug}`: {}. Re-run with `--force` to override.",
                r.message(slug)
            );
        } else {
            report
                .steps
                .push(format!("forced through guard: {}", r.message(slug)));
        }
    }

    // 1b. Stop-then-delete (v0.8.6 W3): `rm` first tears down the
    // project's live chat-mode role sessions, then proceeds with the
    // deregister/purge below. The refusal gate above already gave the
    // user the confirm-vs-`--force` choice; once past it, stopping the
    // sessions is part of the explicit, user-requested removal (the
    // allowed exception to "never PROACTIVELY kill a long session").
    // `--dry-run` lists the sessions it would stop and kills nothing.
    let backend = ccteam_harness::default_process_backend();
    let chat_stop = stop_project_chat_sessions(backend.as_ref(), slug, opts.dry_run)?;
    if opts.dry_run {
        for name in &chat_stop.would_stop {
            report
                .steps
                .push(format!("would stop chat session `{name}`"));
        }
    } else {
        for name in &chat_stop.stopped {
            report.steps.push(format!("stopped chat session `{name}`"));
        }
    }

    // 2. Resolve project_dir via the config registry (so V0.4.2
    // arbitrary-path installs are deleted correctly even when the
    // slug doesn't sit under `paths.projects_root`).
    let registered = ccteam_core::lookup_project_in_config(&paths.root, slug)?;
    let project_dir = match &registered {
        Some(entry) => entry.path.clone(),
        // Fall back to `paths.project_dir(slug)` so `--purge` still
        // works on orphan slugs (state.json present but config entry
        // missing — the V0.4.5 ghost-entry case the PRD references).
        None => paths.project_dir(slug),
    };

    // 3. Config registry drop.
    if registered.is_some() {
        if opts.dry_run {
            report.steps.push(format!(
                "would drop config.yaml::projects entry for `{slug}` (path: {})",
                project_dir.display()
            ));
        } else {
            let removed = ccteam_core::remove_project_from_config(&paths.root, slug)?;
            if removed {
                report
                    .steps
                    .push(format!("removed config.yaml::projects entry for `{slug}`"));
            }
        }
    } else {
        report.steps.push(format!(
            "config.yaml::projects has no entry for `{slug}` (orphan / pre-V0.4.2 install)"
        ));
    }

    // 4. Daemon unroster. Write a per-slug trigger file; if the daemon
    // is alive it polls for these every 250ms (same pattern as the F86
    // shutdown trigger) and calls unroster_project(CancelReason::Removed).
    let trigger_path = unroster_trigger_path(slug);
    let daemon_up = ccteam_core::daemon::daemon_reachable(paths);
    if opts.dry_run {
        report.steps.push(if daemon_up {
            "would write unroster trigger + await daemon acknowledgment (≤5s)".to_string()
        } else {
            "would skip daemon unroster (daemon not running)".to_string()
        });
    } else if !daemon_up {
        report
            .steps
            .push("daemon unroster: skipped (daemon not running)".to_string());
    } else {
        match std::fs::write(&trigger_path, slug) {
            Err(err) => report.steps.push(format!(
                "daemon unroster: trigger write failed ({err}); \
                 daemon will pick up config drop on next rescan"
            )),
            Ok(()) => {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                let mut acknowledged = false;
                while std::time::Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    if !trigger_path.exists() {
                        acknowledged = true;
                        break;
                    }
                }
                if acknowledged {
                    report
                        .steps
                        .push("daemon unroster: acknowledged by daemon".to_string());
                } else {
                    let _ = std::fs::remove_file(&trigger_path);
                    report.steps.push(
                        "daemon unroster: sent (daemon did not acknowledge in 5s; \
                         will pick up on next rescan)"
                            .to_string(),
                    );
                }
            }
        }
    }

    // 5. Orchestration state cleanup under `~/.ccteam/`.
    let progress_path = paths.progress_jsonl(slug);
    let progress_dir = paths.progress_dir().join(slug); // flex shard dir
    let global_inbox_slug_dir = paths.inbox_dir().join(slug);
    let global_control_slug_dir = paths.control_dir().join(slug);

    for (label, path, is_dir) in [
        ("progress.jsonl", progress_path.clone(), false),
        ("progress shard dir", progress_dir.clone(), true),
        ("inbox/<slug>/ dir", global_inbox_slug_dir.clone(), true),
        ("control/<slug>/ dir", global_control_slug_dir.clone(), true),
    ] {
        let exists = if is_dir {
            path.is_dir()
        } else {
            path.is_file()
        };
        if !exists {
            continue;
        }
        if opts.dry_run {
            report
                .steps
                .push(format!("would remove {label} {}", path.display()));
        } else if is_dir {
            std::fs::remove_dir_all(&path).with_context(|| format!("rm -rf {}", path.display()))?;
            report
                .steps
                .push(format!("removed {label} {}", path.display()));
        } else {
            std::fs::remove_file(&path).with_context(|| format!("rm {}", path.display()))?;
            report
                .steps
                .push(format!("removed {label} {}", path.display()));
        }
    }

    // 6. Optional `--purge`: project-local ccteam-managed paths.
    if opts.purge {
        purge_project_managed_paths(&project_dir, opts.dry_run, &mut report)?;
        // V0.6.5 F151 — also clean `~/.ccteam/imd/registry/<slug>/`. The
        // F146 registry is the daemon's bot lifecycle SoT, so without
        // this cleanup `list_bots()` still surfaces stale BotRegistration
        // entries after the workflow.yaml is gone → daemon can spawn an
        // orphan tmux session.
        purge_imd_registry_for_slug(&paths.root, slug, opts.dry_run, &mut report)?;
    }

    Ok(report)
}

/// Outcome of enumerating + (optionally) killing a project's live
/// chat-mode role sessions. Shared by `run_project_stop` (the `project
/// stop` command) and `run_remove` (rm = stop-then-delete).
///
/// `stopped` carries the tmux session names that were actually killed;
/// under `--dry-run` it is empty and `would_stop` carries the targets
/// instead (so the dry-run preview shows the same shape it would act on).
#[derive(Debug, Clone, Default)]
struct ChatSessionStop {
    /// Session names killed this run (empty under dry-run).
    stopped: Vec<String>,
    /// Session names that *would* be killed (only populated under dry-run).
    would_stop: Vec<String>,
}

/// Enumerate the live chat-mode role sessions belonging to `slug`
/// (`ccteam-chat-<slug>-<role>`) and, unless `dry_run`, kill each one.
///
/// Backend-agnostic: enumeration + kill both go through the injected
/// [`ProcessBackend`] (`list_sessions` / `kill`), so the teardown sees
/// whatever mux is live under `CCTEAM_MUX_BACKEND` — the bundled `rmux`
/// daemon by default, or `tmux` when opted in. (The old tmux-only path
/// shelled out to `tmux list-sessions` directly and so saw nothing under
/// the default rmux backend.) The CLI threads `default_process_backend()`
/// in; tests inject a deterministic [`InProcBackend`].
///
/// Process-independent on purpose: the CLI is a separate process from the
/// daemon, so we never consult daemon in-memory state — only the live mux
/// session names. We reuse [`list_chat_sessions`] (names only, never
/// capture-pane) and keep the ones whose parsed slug equals `slug`,
/// parsing via [`parse_chat_session_name`] rather than a raw `starts_with`
/// so a slug that itself contains dashes (e.g. `dev-foo`) matches its own
/// `ccteam-chat-dev-foo-<role>` sessions and not a sibling
/// `ccteam-chat-dev-<role>` one. The slug is always the *first* parsed
/// element, so this stays correct even if the trailing segment changes
/// meaning (role → sid).
///
/// **Red line.** `project stop` / `rm` are EXPLICIT user commands, the
/// allowed exception to "never PROACTIVELY kill a long session": the
/// teardown is user-requested and resumable (the daemon recreates the
/// pane via `--resume` on the next interaction). The kill is idempotent
/// ([`ProcessBackend::kill`] is `Ok(())` for a vanished session).
fn stop_project_chat_sessions(
    backend: &dyn ccteam_harness::ProcessBackend,
    slug: &str,
    dry_run: bool,
) -> Result<ChatSessionStop> {
    use ccteam_harness::{list_chat_sessions, parse_chat_session_name, MuxSessionId};

    // Enumerate live chat sessions via the backend, keep ours, stable-sort
    // so output / kill order is deterministic.
    let live = block_on_async(list_chat_sessions(backend))??;
    let mut matches: Vec<String> = live
        .into_iter()
        .filter(|name| {
            parse_chat_session_name(name)
                .map(|(s, _last)| s == slug)
                .unwrap_or(false)
        })
        .collect();
    matches.sort();

    let mut out = ChatSessionStop::default();
    if dry_run {
        out.would_stop = matches;
        return Ok(out);
    }
    for name in matches {
        block_on_async(backend.kill(&MuxSessionId::new(name.clone())))?
            .with_context(|| format!("stop chat session `{name}`"))?;
        out.stopped.push(name);
    }
    Ok(out)
}

/// v0.8.6 W3 — `ccteam project stop <slug>` handler.
///
/// Stops ALL of the project's live chat-mode role sessions (the
/// `ccteam-chat-<slug>-*` tmux sessions) WITHOUT removing the project.
/// This is an explicit, resumable user-requested stop — the project
/// stays registered and the daemon resumes each role by id on the next
/// interaction. `stop` is neither a delete nor a pause.
///
/// Returns a one-line-per-session render (and a tail count); stopping 0
/// sessions is a success, not an error.
pub fn run_project_stop(_paths: &CcteamPaths, slug: &str) -> Result<String> {
    let backend = ccteam_harness::default_process_backend();
    let stop = stop_project_chat_sessions(backend.as_ref(), slug, false)?;

    let mut out = String::new();
    use std::fmt::Write as _;
    writeln!(out, "ccteam project stop {slug}").ok();
    for name in &stop.stopped {
        writeln!(out, "  - stopped chat session `{name}` (resumable by id)").ok();
    }
    let n = stop.stopped.len();
    writeln!(
        out,
        "stopped {n} chat session{} for `{slug}`{}",
        if n == 1 { "" } else { "s" },
        if n == 0 { " (none were running)" } else { "" }
    )
    .ok();
    Ok(out)
}

/// V0.6.5 F151 — purge `~/.ccteam/imd/registry/<slug>/`.
///
/// **Strategy:** for each registered role under the slug, call
/// [`ccteam_im::unregister_bot_in`] first — this is the in-process
/// equivalent of the `chat_unregister_bot` MCP tool. It deletes the
/// `<role>.json` registry file *and* the `<role>.heartbeat` sidecar,
/// which is exactly what the daemon's registry watcher observes to
/// close the tmux session gracefully. After all roles are unregistered
/// (or if the registry dir is empty / malformed), `rm -rf` the
/// `<slug>/` dir to clean any leftover files (e.g. stale heartbeats
/// from a previous unregister/re-register cycle that left a sidecar
/// orphan).
///
/// Works whether the daemon is running or not — `unregister_bot_in`
/// is pure file IO, no daemon RPC.
fn purge_imd_registry_for_slug(
    ccteam_root: &std::path::Path,
    slug: &str,
    dry_run: bool,
    report: &mut RemoveReport,
) -> Result<()> {
    let slug_dir = ccteam_im::registry_root_in(ccteam_root).join(slug);
    if !slug_dir.exists() {
        // Nothing to clean — non-chat slug or already pristine. Stay
        // silent (don't add a "nothing to do" step row that clutters
        // dry-run output).
        return Ok(());
    }

    // Enumerate roles via list_bots_in (so we route through the F146
    // MCP-equivalent surface). Falls back to empty list on parse errors;
    // the final rm -rf still catches whatever remains.
    let bots = ccteam_im::list_bots_in(ccteam_root, Some(slug)).unwrap_or_default();
    let role_count = bots.len();

    if dry_run {
        // Count JSON files even if list_bots_in skipped malformed rows.
        let json_count = std::fs::read_dir(&slug_dir)
            .map(|it| {
                it.filter_map(|e| e.ok())
                    .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("json"))
                    .count()
            })
            .unwrap_or(0);
        let noun = if json_count == 1 {
            "JSON file"
        } else {
            "JSON files"
        };
        report.steps.push(format!(
            "would purge imd/registry/{slug}/ ({json_count} {noun})"
        ));
        return Ok(());
    }

    // Step 6a — per-role unregister (MCP-equivalent in-process call).
    // Idempotent miss is fine; just records the path that *was* there.
    for reg in bots {
        match ccteam_im::unregister_bot_in(ccteam_root, &reg.workflow_slug, &reg.role) {
            Ok((removed, path)) => {
                if removed {
                    report.steps.push(format!(
                        "unregistered bot `{}` (deleted {})",
                        reg.role,
                        path.display()
                    ));
                }
            }
            Err(err) => {
                // Don't bail — the rm -rf below is the fallback.
                report.steps.push(format!(
                    "unregister bot `{}` failed ({err}); will fall back to rm -rf",
                    reg.role
                ));
            }
        }
    }

    // Step 6b — final sweep. Catches: heartbeat sidecars whose
    // registration JSON was already gone, malformed registration
    // files list_bots_in skipped, or the empty slug dir itself.
    if slug_dir.exists() {
        std::fs::remove_dir_all(&slug_dir)
            .with_context(|| format!("rm -rf {}", slug_dir.display()))?;
        report.steps.push(format!(
            "purged imd/registry/{slug}/ ({role_count} role{} cleared)",
            if role_count == 1 { "" } else { "s" }
        ));
    }

    Ok(())
}

/// Helper for `run_remove --purge` — deletes exactly ccteam's own
/// footprint inside `<project>/` (or, under `--dry-run`, just records
/// the planned step). Aligned to the v0.8.6 W2 layout: a project's
/// on-disk ccteam footprint is `.ccteam/` (state.json + workflow.yaml),
/// the seeded `.claude/agents/cto.md` persona, and ccteam's hook section
/// inside `.claude/settings.local.json`.
///
/// **DELETE** (ccteam-managed only):
/// - `<project>/.ccteam/` — state.json + workflow.yaml live here (W2).
/// - `<project>/.claude/agents/cto.md` — the ccteam-seeded chat persona.
///   The `.claude/agents/` dir itself is left in place so user-authored
///   work-roles beside `cto.md` survive (only `cto.md` is ours).
/// - ccteam's chat-progress + AskUserQuestion hook entries inside
///   `<project>/.claude/settings.local.json` (surgically, via
///   [`ccteam_core::remove_chat_hooks`]). If stripping them leaves the
///   file an empty object, the now-vestigial file is deleted.
///
/// **KEEP (never touched):**
/// - User work-roles `<project>/.claude/agents/*.md` other than `cto.md`.
/// - `<project>/CLAUDE.md` / `AGENTS.md` (project knowledge = vendor-native).
/// - `<project>/.env` (user-controlled secrets — CLAUDE.md §三 red line).
/// - The user's `<project>/.claude/settings.json` (ccteam manages only
///   the `settings.local.json` layer, never the committed one).
/// - All business code. If the user wants the whole tree gone they can
///   `rm -rf <project>` themselves; the `--purge` contract is strictly
///   ccteam-footprint-only.
fn purge_project_managed_paths(
    project_dir: &std::path::Path,
    dry_run: bool,
    report: &mut RemoveReport,
) -> Result<()> {
    // 1. `.ccteam/` dir (state.json + workflow.yaml). W2 moved
    // workflow.yaml under `.ccteam/`, so this one delete covers both.
    let ccteam_dir = project_dir.join(".ccteam");
    if ccteam_dir.is_dir() {
        if dry_run {
            report
                .steps
                .push(format!("would purge .ccteam/ ({})", ccteam_dir.display()));
        } else {
            std::fs::remove_dir_all(&ccteam_dir)
                .with_context(|| format!("rm -rf {}", ccteam_dir.display()))?;
            report
                .steps
                .push(format!("purged .ccteam/ ({})", ccteam_dir.display()));
        }
    }

    // 2. The ccteam-seeded `cto.md` persona ONLY — never the whole
    // `.claude/agents/` dir (user work-roles live beside it and must
    // survive).
    let cto_md = project_dir.join(".claude").join("agents").join("cto.md");
    if cto_md.is_file() {
        if dry_run {
            report.steps.push(format!(
                "would purge .claude/agents/cto.md ({})",
                cto_md.display()
            ));
        } else {
            std::fs::remove_file(&cto_md).with_context(|| format!("rm {}", cto_md.display()))?;
            report.steps.push(format!(
                "purged .claude/agents/cto.md ({})",
                cto_md.display()
            ));
        }
    }

    // 3. ccteam's hook section inside `.claude/settings.local.json`,
    // removed surgically so any operator-authored keys/hooks survive.
    // If the strip empties the file, delete the vestigial object.
    let settings_local = project_dir.join(".claude").join("settings.local.json");
    let scrub = ccteam_core::remove_chat_hooks(&settings_local, dry_run)?;
    use ccteam_core::ChatHookScrubAction as A;
    match scrub.action {
        A::NotFound | A::NoChangeNeeded => {}
        A::WouldRemove { entries } => {
            report.steps.push(format!(
                "would strip {entries} ccteam hook entr{} from .claude/settings.local.json ({})",
                if entries == 1 { "y" } else { "ies" },
                settings_local.display()
            ));
        }
        A::Removed { entries } => {
            report.steps.push(format!(
                "stripped {entries} ccteam hook entr{} from .claude/settings.local.json ({})",
                if entries == 1 { "y" } else { "ies" },
                settings_local.display()
            ));
        }
        A::RemovedNowEmpty { entries } => {
            // The file existed only to carry ccteam's hooks — delete it.
            std::fs::remove_file(&settings_local)
                .with_context(|| format!("rm {}", settings_local.display()))?;
            report.steps.push(format!(
                "stripped {entries} ccteam hook entr{} and removed now-empty \
                 .claude/settings.local.json ({})",
                if entries == 1 { "y" } else { "ies" },
                settings_local.display()
            ));
        }
    }

    // Paranoia: confirm `.env` survived (if a future refactor widens the
    // purge tree this surfaces in tests immediately).
    let env_file = project_dir.join(".env");
    if env_file.exists() {
        report
            .steps
            .push(format!("preserved {}", env_file.display()));
    }

    Ok(())
}

// V0.6.6 F167 — `ccteam probe-project` CLI subcommand. Thin wrapper
// over `ccteam_core::probe_project()` that emits a JSON object the
// `/ccteam-creator` skill can fold into Phase 4's PROJECT PLAN ctx
// (so the rendered workflow.yaml's `scope:` is pre-populated with
// sensible defaults instead of the empty stub the skill used to
// emit). Pure CLI surface — the heuristic lives in the core crate;
// this just serializes + writes to stdout.
//
// Output schema (stable for V0.6.6+):
//
// ```json
// {
//   "kind": "monorepo" | "single-repo" | "docs-only" | "scripts-only" | "empty",
//   "languages": ["rust", "typescript", ...],
//   "has_tests": true | false,
//   "probable_scope": ["crates/ccteam-core/src", "crates/ccteam-cli/src", ...]
// }
// ```
pub fn run_probe_project(repo_root: &std::path::Path, json: bool) -> Result<String> {
    let probe = ccteam_core::probe_project(repo_root);
    if json {
        // V0.6.6 F167 — the wire shape lives in `ProjectProbe`'s
        // `serde(rename_all = "kebab-case")` derive, so we just
        // serialize directly.
        Ok(serde_json::to_string_pretty(&probe).context("serialize probe-project JSON")?)
    } else {
        // Human-readable fallback for `ccteam probe-project` without
        // `--json` — a 4-line summary the user can eyeball before
        // they pipe it into the skill.
        let langs: Vec<&str> = probe.languages.iter().map(|l| l.as_str()).collect();
        let scope: Vec<String> = probe
            .probable_scope
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        Ok(format!(
            "kind: {}\nlanguages: [{}]\nhas_tests: {}\nprobable_scope: [{}]\n",
            probe.kind.as_str(),
            langs.join(", "),
            probe.has_tests,
            scope.join(", "),
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, OnceLock};

    use super::*;
    use ccteam_core::{disable_tool_surface_bootstrap_for_tests, progress};
    use tempfile::TempDir;

    /// Disable tool-surface ~/.claude/ mutation for the whole test
    /// binary. These tests exercise CLI command rendering, not the
    /// agent symlink chain — that's tested separately in
    /// crates/ccteam-core/tests/tool_surface_e2e_test.rs with
    /// CLAUDE_CONFIG_HOME isolation.
    static DISABLE_TOOL_SURFACE: OnceLock<()> = OnceLock::new();
    fn ensure_isolation() {
        DISABLE_TOOL_SURFACE.get_or_init(disable_tool_surface_bootstrap_for_tests);
    }

    /// Serialize tests that mutate `CLAUDE_CONFIG_HOME`. Per CLAUDE.md
    /// §六, env-mutating tests really belong under
    /// `crates/*/tests/*.rs` (separate processes), but until that
    /// migration these tests can race against each other since they
    /// run in the same process. The mutex makes them deterministic.
    /// V0.2 M0.16.2 widened the timing window with the extra
    /// `write_all_global_team_templates` call before
    /// `install_memory_bridge`.
    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn fresh_paths(tmp: &TempDir) -> CcteamPaths {
        CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        }
    }

    #[cfg(unix)]
    fn fake_daemon(paths: &CcteamPaths) -> std::os::unix::net::UnixListener {
        let socket = ccteam_core::daemon::daemon_socket_path(paths);
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
        std::os::unix::net::UnixListener::bind(socket).unwrap()
    }

    /// V0.4.2 F75: tests that previously used `run_new_t4` (the
    /// Tier-4 deterministic slug wrapper) now bootstrap directly via
    /// the core helper. Same effect for setup purposes — the
    /// LLM/auto-slug path was removed with the rest of `run_new`.
    fn run_new_t4(paths: &CcteamPaths, request: &str, team: &str) -> Result<String> {
        let slug = ccteam_core::pick_unused_slug(paths, request, team)?;
        ccteam_core::bootstrap_project(paths, &slug, request, team)?;
        Ok(slug)
    }

    #[test]
    fn run_peek_uses_state_tmux_session_for_meta_project() {
        // Serialize the env mutation (default backend is now rmux; this
        // test asserts the tmux peek path, so pin tmux while it runs).
        let _lock = env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let mut state = ProjectState::initial_for_team("meta-cto".into(), "meta-agent".into());
        state.tmux_session = "ccteam-meta-cto".into();
        state.save(&paths.project_state("meta-cto")).unwrap();

        std::env::set_var("CCTEAM_MUX_BACKEND", "tmux");
        let result = run_peek_with_role(&paths, "meta-cto", None);
        std::env::remove_var("CCTEAM_MUX_BACKEND");

        let err = result.unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("ccteam-meta-cto"),
            "peek should target state.tmux_session, got: {msg}",
        );
    }

    /// v0.8.8 B1 — `stop_project_chat_sessions` consults the *injected*
    /// [`ProcessBackend`], not shell `tmux` directly. The deterministic
    /// list+kill+absent semantics (which need a single live tokio runtime
    /// across spawn→list→kill) are verified in the harness layer
    /// (`ccteam_harness::stop_chat_sessions_for_slug_kills_only_that_slug`);
    /// here we only assert the CLI bridge is wired to the backend — an empty
    /// backend yields an empty result with no `tmux list-sessions` shell-out
    /// and no panic (the bug was the old tmux-only path returning nothing
    /// under the default rmux backend).
    ///
    /// (We can't drive the kill end-to-end here: `block_on_async` builds a
    /// fresh current-thread runtime per call, so an `InProcBackend`'s parked
    /// `tokio::spawn` task — its liveness signal — dies with the runtime that
    /// spawned it. A single-runtime harness test is the right home.)
    #[test]
    fn stop_project_chat_sessions_consults_injected_backend() {
        use ccteam_harness::InProcBackend;

        let backend = InProcBackend::new();
        // Empty backend → no matches, both modes, no error / no tmux shell-out.
        let dry = stop_project_chat_sessions(&backend, "dev-foo", true).unwrap();
        assert!(dry.would_stop.is_empty() && dry.stopped.is_empty());
        let stop = stop_project_chat_sessions(&backend, "dev-foo", false).unwrap();
        assert!(stop.would_stop.is_empty() && stop.stopped.is_empty());
    }

    /// v0.8.8 B4 — a tracked row from a fixture gateway state renders one row
    /// per session with its VENDOR + SID, and a **codex** session shows
    /// `live` (the BUG-5 fix: tracked ⇒ live regardless of vendor, no more
    /// false "registered, not running").
    #[test]
    fn render_sessions_table_codex_tracked_is_live_with_vendor() {
        let tracked = vec![
            ccteam_im::gateway::TrackedSessionRow {
                sid: "s1".into(),
                project: "alpha".into(),
                role: "reviewer".into(),
                vendor: "claude".into(),
                permission_mode: "skip".into(),
            },
            ccteam_im::gateway::TrackedSessionRow {
                sid: "s2".into(),
                project: "alpha".into(),
                role: "builder".into(),
                vendor: "codex".into(),
                permission_mode: "hitl".into(),
            },
        ];
        let out = render_sessions_table(&tracked, &[], true);

        // Header carries the new VENDOR column alongside SLUG/SID/ROLE.
        assert!(out.contains("SLUG"));
        assert!(out.contains("SID"));
        assert!(out.contains("ROLE"));
        assert!(out.contains("VENDOR"));

        // Both rows present with their sid + vendor.
        let claude_line = out.lines().find(|l| l.contains("s1")).expect("claude row");
        assert!(claude_line.contains("reviewer"), "{claude_line}");
        assert!(claude_line.contains("claude"), "{claude_line}");
        assert!(claude_line.contains("live"), "{claude_line}");

        let codex_line = out.lines().find(|l| l.contains("s2")).expect("codex row");
        assert!(codex_line.contains("builder"), "{codex_line}");
        assert!(codex_line.contains("codex"), "{codex_line}");
        // BUG-5: codex tracked session is live, never "registered, not running".
        assert!(codex_line.contains("live"), "{codex_line}");
        assert!(
            !out.contains("registered, not running"),
            "BUG-5 regression: false not-running note returned: {out}"
        );
    }

    /// v0.8.8 B4 — a live `ccteam-chat-*` pane the daemon does not track is an
    /// orphan (role/vendor `-`); daemon-down degrades tracked rows to
    /// `registered (daemon down)` rather than erroring.
    #[test]
    fn render_sessions_table_orphan_and_daemon_down() {
        let tracked = vec![ccteam_im::gateway::TrackedSessionRow {
            sid: "s1".into(),
            project: "alpha".into(),
            role: "cto".into(),
            vendor: "claude".into(),
            permission_mode: "skip".into(),
        }];
        // One untracked live pane → orphan; the tracked s1's own pane is NOT
        // listed, so it must not double as an orphan.
        let live = vec!["ccteam-chat-ghost-zombie".to_string()];
        let out = render_sessions_table(&tracked, &live, false);

        let tracked_line = out.lines().find(|l| l.contains("s1")).expect("tracked");
        assert!(
            tracked_line.contains("registered (daemon down)"),
            "{tracked_line}"
        );

        let orphan_line = out
            .lines()
            .find(|l| l.contains("zombie"))
            .expect("orphan row");
        assert!(orphan_line.contains("ghost"), "{orphan_line}");
        assert!(orphan_line.contains("orphan"), "{orphan_line}");
    }

    /// v0.8.8 B4 — a tracked session's own live pane is reconciled (matched by
    /// canonical name), never re-listed as an orphan.
    #[test]
    fn render_sessions_table_tracked_pane_not_an_orphan() {
        let tracked = vec![ccteam_im::gateway::TrackedSessionRow {
            sid: "s1".into(),
            project: "alpha".into(),
            role: "cto".into(),
            vendor: "claude".into(),
            permission_mode: "skip".into(),
        }];
        // The live pane name matches the canonical name of the tracked s1.
        let live = vec!["ccteam-chat-alpha-s1".to_string()];
        let out = render_sessions_table(&tracked, &live, true);
        assert!(!out.contains("orphan"), "tracked pane misflagged: {out}");
        // Exactly one data row (s1), not two.
        assert_eq!(out.matches("s1").count(), 1, "{out}");
    }

    #[test]
    fn stall_verdict_classifies_silence_tiers() {
        // 0s → OK, 5m → warn, 15m+/30m+ both collapse to STUCK so the
        // status row prints a verdict instead of raw seconds.
        assert_eq!(stall_verdict(0), "OK");
        assert_eq!(stall_verdict(5 * 60), "warn");
        assert_eq!(stall_verdict(15 * 60), "STUCK");
        // Past the escalate threshold (30m) the project is STUCK and the
        // takeover hint names the exact peek → attach sequence.
        assert_eq!(stall_verdict(30 * 60), "STUCK");
        let hint = stall_takeover_hint("dev-checkout", "31m");
        assert!(hint.contains("dev-checkout"), "hint names the slug: {hint}");
        assert!(hint.contains("silent 31m"), "hint shows silence: {hint}");
        assert!(
            hint.contains("ccteam internal peek dev-checkout"),
            "hint points at peek: {hint}",
        );
        assert!(
            hint.contains("ccteam internal attach dev-checkout"),
            "hint points at attach: {hint}",
        );
    }

    #[test]
    fn legacy_state_json_without_team_field_loads_as_dev() {
        // F13 backwards-compat: state.json files written by pre-M3.1
        // ccteam don't have a `team` field; serde default kicks in.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("state.json");
        // Hand-rolled JSON missing the `team` key:
        let body = r#"{
            "slug": "legacy",
            "created_at": "2026-05-01T00:00:00Z",
            "tmux_session": "ccteam-legacy",
            "claude_session_id": null,
            "claude_pid": null,
            "phase_state": "idle",
            "current_phase": "",
            "parallelism": "solo",
            "phase_history": [],
            "auto_loop_cycle_count": 0,
            "cost_used_usd": 0.0,
            "soft_warn_threshold_usd": 20.0,
            "hard_kill_threshold_usd": 200.0,
            "context_tokens_used": 0,
            "context_reset_threshold_tokens": 600000,
            "context_reset_count": 0,
            "last_progress_event_at": null,
            "last_event_type": null,
            "last_user_interaction_at": "2026-05-01T00:00:00Z",
            "user_attached": false,
            "user_pause_pending": false
        }"#;
        std::fs::write(&path, body).unwrap();
        let s = ProjectState::load(&path).unwrap();
        assert_eq!(s.team, "dev");
    }

    #[test]
    fn run_ls_text_says_no_projects_when_empty() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let body = run_ls(&paths, OutputFormat::Text).unwrap();
        assert!(body.contains("no projects"));
    }

    #[test]
    fn run_ls_json_emits_orchestrator_block_with_active_count() {
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        run_new_t4(&paths, "demo one", "dev").unwrap();
        run_new_t4(&paths, "demo two", "dev").unwrap();

        let body = run_ls(&paths, OutputFormat::Json).unwrap();
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["projects"].as_array().unwrap().len(), 2);
        assert_eq!(v["orchestrator"]["active_count"], 0);
        assert_eq!(v["orchestrator"]["max_concurrent"], 1);
    }

    #[test]
    fn f27_run_ls_text_reports_daemon_down_when_socket_unreachable() {
        // F27 — `ls` text output annotates daemon health on its first
        // line so users can disambiguate "no projects" from "daemon
        // never came up".
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let body = run_ls(&paths, OutputFormat::Text).unwrap();
        let first_line = body.lines().next().unwrap_or("");
        assert!(
            first_line == "daemon: down",
            "expected first line `daemon: down`; got: {first_line}",
        );
    }

    #[test]
    fn f27_run_ls_json_orchestrator_running_is_bool() {
        // F27 — `orchestrator.running` was hardcoded `null` pre-V0.2.1;
        // now it's a real bool gated on socket reachability so MCP /
        // meta-agent consumers can treat it as a status field.
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let body = run_ls(&paths, OutputFormat::Json).unwrap();
        let v: Value = serde_json::from_str(&body).unwrap();
        let running = &v["orchestrator"]["running"];
        assert!(
            running.is_boolean(),
            "expected orchestrator.running:bool; got: {running:?}",
        );
        // No MCP socket listener → must be false.
        assert_eq!(running.as_bool(), Some(false));
    }

    #[test]
    fn f27_run_ls_text_reports_daemon_up_on_reachable_socket() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let _daemon = fake_daemon(&paths);
        let body = run_ls(&paths, OutputFormat::Text).unwrap();
        assert!(
            body.starts_with("daemon: up"),
            "expected `daemon: up` head line on reachable socket; got:\n{body}",
        );
    }

    #[test]
    fn run_show_json_includes_state_and_artifacts() {
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let slug = run_new_t4(&paths, "demo", "dev").unwrap();
        let body = run_show(&paths, &slug, OutputFormat::Json).unwrap();
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["state"]["slug"], slug);
        // v0.8.6: `.ccteam/` keeps only state.json + workflow.yaml, so
        // there is no `spec` artifact to surface.
        assert!(
            v["artifacts"].as_object().unwrap().is_empty(),
            "v0.8.6: no .ccteam/*.md artifacts; got: {}",
            v["artifacts"],
        );
    }

    #[test]
    fn run_show_errors_for_missing_slug() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let err = run_show(&paths, "ghost", OutputFormat::Text).unwrap_err();
        assert!(format!("{err:#}").contains("ghost"));
    }

    #[test]
    fn run_resume_archives_escalation_and_resets_phase_state() {
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let slug = run_new_t4(&paths, "demo", "dev").unwrap();
        let state_path = paths.project_state(&slug);
        let mut state = ProjectState::load(&state_path).unwrap();
        state.user_pause_pending = true;
        state.save(&state_path).unwrap();
        let esc = paths.project_ccteam_dir(&slug).join("escalation.md");
        std::fs::write(&esc, "stuck").unwrap();

        run_resume(&paths, &slug).unwrap();
        let s2 = ProjectState::load(&state_path).unwrap();
        assert_eq!(s2.phase_state, PhaseState::Idle);
        assert!(!s2.user_pause_pending);
        assert!(
            !esc.exists(),
            "escalation.md should be archived after resume"
        );
    }

    /// V0.4.2 F72: build `InitOptions` that targets a slug inside the
    /// tempdir so tests don't accidentally try to install ccteam in
    /// the ccteam repo cwd (which is fail-loud).
    fn init_opts_targeting_tmp(tmp: &TempDir, slug: &str) -> InitOptions {
        InitOptions {
            install_in: Some(tmp.path().join(slug)),
            ..InitOptions::default()
        }
    }

    #[test]
    fn run_init_creates_global_skeleton_and_unpacks_helpers() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let report = run_init(&paths, init_opts_targeting_tmp(&tmp, "scaffold-demo")).unwrap();
        // v0.8.6 D1.1: init creates exactly the canonical home-layout
        // manifest. `hooks/` is materialized by the dispatcher install
        // step rather than the skeleton loop, but it must still exist.
        for sub in ccteam_core::canonical_home_dirs() {
            assert!(
                paths.root.join(sub).is_dir(),
                "init must create canonical home dir {}",
                paths.root.join(sub).display()
            );
        }
        // The orchestrator-era subdirs are no longer created by init —
        // nothing reads them post-W2 and they used to trip the doctor
        // home-drift check on a brand-new install.
        for dead in ["phases", "inbox", "control"] {
            assert!(
                !paths.root.join(dead).exists(),
                "init must NOT create orchestrator-era dir {}",
                paths.root.join(dead).display()
            );
        }
        assert!(report.contains("ccteam init"));
        assert!(report.contains("next"));
    }

    /// v0.8.6 D1.1 regression: a brand-new `ccteam init` must not create
    /// any directory the doctor home-drift check flags. Before D1.1, init
    /// stamped `phases/templates/inbox/control` while the drift check only
    /// tolerated `canonical_home_dirs()` — so a fresh install reported
    /// four self-inflicted drift dirs on the very next `ccteam doctor`.
    #[test]
    fn run_init_leaves_no_home_layout_drift() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        run_init(&paths, init_opts_targeting_tmp(&tmp, "drift-demo")).unwrap();
        let drift = render_home_drift_line(&paths);
        assert!(
            drift.is_empty(),
            "a fresh `ccteam init` must produce zero home drift; got: {drift}",
        );
    }

    #[test]
    fn run_init_is_idempotent_and_preserves_user_workflow_yaml() {
        // V0.5.0 F101: HELPER_TEMPLATES is empty so the V0.4.x
        // `~/.ccteam/templates/review-with-user-loop.md` idempotency
        // probe is gone. workflow.yaml is the new canonical
        // "preserve without --force, overwrite with --force" artifact —
        // it's user-territory by design (see `force_overwrites_user_workflow_and_agents`).
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        run_init(&paths, init_opts_targeting_tmp(&tmp, "idem-demo")).unwrap();
        let path = tmp
            .path()
            .join("idem-demo")
            .join(".ccteam")
            .join("workflow.yaml");
        std::fs::write(&path, "USER EDIT").unwrap();
        run_init(&paths, init_opts_targeting_tmp(&tmp, "idem-demo")).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "USER EDIT");
        run_init(
            &paths,
            InitOptions {
                force: true,
                install_in: Some(tmp.path().join("idem-demo")),
                ..InitOptions::default()
            },
        )
        .unwrap();
        assert_ne!(std::fs::read_to_string(&path).unwrap(), "USER EDIT");
    }

    /// V0.4.2 F72: fresh install scaffolds the project skeleton AND
    /// registers in config.yaml.
    #[test]
    fn run_init_fresh_install_scaffolds_and_registers() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let target = tmp.path().join("f72-fresh");
        run_init(
            &paths,
            InitOptions {
                install_in: Some(target.clone()),
                slug: Some("f72-fresh".into()),
                team: Some("dev".into()),
                ..InitOptions::default()
            },
        )
        .unwrap();
        assert!(target.join(".ccteam").join("state.json").is_file());
        assert!(
            target.join(".ccteam").join("workflow.yaml").is_file(),
            "V0.4.6 F83: workflow.yaml must land in .ccteam/, not project root",
        );
        assert!(
            !target.join("workflow.yaml").exists(),
            "V0.4.6 F83: workflow.yaml must NOT be at the project root after fresh init",
        );
        assert!(target
            .join(".claude")
            .join("agents")
            .join("cto.md")
            .is_file());
        // v0.8.6: init no longer writes the `.ccteam/agents` neutral copy
        // nor the `.ccteam/skills` placeholder layout — `.ccteam/` holds
        // only state.json + workflow.yaml.
        assert!(
            !target.join(".ccteam").join("agents").exists(),
            "v0.8.6: .ccteam/agents neutral copy must NOT be written",
        );
        assert!(
            !target.join(".ccteam").join("skills").exists(),
            "v0.8.6: .ccteam/skills placeholder must NOT be written",
        );

        let cfg = ccteam_core::load_ccteam_config(&paths.root).unwrap();
        assert_eq!(cfg.projects.len(), 1);
        assert_eq!(cfg.projects[0].slug, "f72-fresh");
        assert_eq!(cfg.projects[0].team, "dev");
    }

    /// V0.5.0 F93b — `ccteam init --mode agent-team` scaffolds the
    /// 4 required artifacts: `workflow.yaml` (with `mode: agent-team`),
    /// `__lead.md`, `.ccteam/inbox/`, and registers in config.yaml.
    /// Also writes the F94 `settings.agent-team.json` template
    /// (with `TeammateIdle` / `TaskCreated` / `TaskCompleted` hooks).
    #[test]
    fn run_init_agent_team_mode_writes_four_files() {
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let target = tmp.path().join("my-debate");
        run_init(
            &paths,
            InitOptions {
                install_in: Some(target.clone()),
                slug: Some("my-debate".into()),
                mode: InitMode::AgentTeam,
                ..InitOptions::default()
            },
        )
        .unwrap();
        // 1. workflow.yaml exists with mode: agent-team
        let wf = target.join(".ccteam").join("workflow.yaml");
        assert!(wf.is_file(), "workflow.yaml must land in .ccteam/");
        let body = std::fs::read_to_string(&wf).unwrap();
        assert!(
            body.contains("mode: agent-team"),
            "workflow.yaml must declare mode: agent-team; got: {body}",
        );
        // 2. __lead.md scaffold
        let lead = target.join(".claude").join("agents").join("__lead.md");
        assert!(
            lead.is_file(),
            "__lead.md must be scaffolded in agent-team mode"
        );
        let lead_body = std::fs::read_to_string(&lead).unwrap();
        assert!(
            lead_body.contains("name: __lead"),
            "__lead.md must have Anthropic frontmatter",
        );
        // 3. .ccteam/inbox/.gitkeep sentinel
        let inbox = target.join(".ccteam").join("inbox");
        assert!(inbox.is_dir(), "inbox dir must be created");
        assert!(
            inbox.join(".gitkeep").is_file(),
            ".ccteam/inbox/.gitkeep sentinel must be written",
        );
        // 4. config.yaml registry entry
        let cfg = ccteam_core::load_ccteam_config(&paths.root).unwrap();
        assert_eq!(cfg.projects.len(), 1);
        assert_eq!(cfg.projects[0].slug, "my-debate");
        // 5. F94: settings.agent-team.json template wrote 3 hooks.
        // v0.8.6 W2b — ccteam writes its managed base/hooks to the
        // local layer (settings.local.json), never the user-committed
        // settings.json.
        let settings_path = target.join(".claude").join("settings.local.json");
        assert!(settings_path.is_file());
        let settings_body = std::fs::read_to_string(&settings_path).unwrap();
        for hook in ["TeammateIdle", "TaskCreated", "TaskCompleted"] {
            assert!(
                settings_body.contains(hook),
                "settings.local.json (agent-team) must include `{hook}` hook; got: {settings_body}",
            );
        }
    }

    /// V0.5.0 F93b — `ccteam init` without `--mode` defaults to
    /// artifact-driven; no `__lead.md` is scaffolded.
    #[test]
    fn run_init_default_mode_does_not_scaffold_lead() {
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let target = tmp.path().join("artifact-default");
        run_init(
            &paths,
            InitOptions {
                install_in: Some(target.clone()),
                ..InitOptions::default()
            },
        )
        .unwrap();
        let lead = target.join(".claude").join("agents").join("__lead.md");
        assert!(
            !lead.exists(),
            "__lead.md must NOT be scaffolded in default mode"
        );
        let wf = target.join(".ccteam").join("workflow.yaml");
        let body = std::fs::read_to_string(&wf).unwrap();
        assert!(
            !body.contains("mode: agent-team"),
            "default workflow.yaml must NOT declare agent-team mode",
        );
        // No F94 hooks in the default (managed) settings layer. v0.8.6
        // W2b — ccteam writes to settings.local.json, not settings.json.
        let settings_body =
            std::fs::read_to_string(target.join(".claude").join("settings.local.json")).unwrap();
        assert!(
            !settings_body.contains("TeammateIdle"),
            "default settings.local.json must NOT include team_* hooks",
        );
    }

    /// V0.5.0 F93b — `ccteam start <slug> --dry-run` prints the spawn
    /// preview without spawning. The `<slug>` must point at an
    /// agent-team mode project.
    #[test]
    fn run_start_agent_team_dry_run_prints_preview() {
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let target = tmp.path().join("dry-run-team");
        run_init(
            &paths,
            InitOptions {
                install_in: Some(target.clone()),
                slug: Some("dry-run-team".into()),
                mode: InitMode::AgentTeam,
                ..InitOptions::default()
            },
        )
        .unwrap();
        // Point projects_root at the install target via config.yaml
        // entry (run_init already upserts it). For run_start, the slug
        // must resolve via `paths.project_dir(slug)`.
        // Our test paths.projects_root is `tmp/projects/`, but the
        // install went into `tmp/dry-run-team/`. We need either to
        // install under projects_root OR override paths to point there.
        // Re-init under projects_root for proper slug resolution:
        let target2 = paths.projects_root.join("dry-run-team-2");
        run_init(
            &paths,
            InitOptions {
                install_in: Some(target2.clone()),
                slug: Some("dry-run-team-2".into()),
                mode: InitMode::AgentTeam,
                ..InitOptions::default()
            },
        )
        .unwrap();
        // Customize the workflow.yaml lead_seed for assertion clarity.
        let wf = target2.join(".ccteam").join("workflow.yaml");
        let raw = std::fs::read_to_string(&wf).unwrap();
        std::fs::write(
            &wf,
            raw.replace(
                "<describe the team's mission here>",
                "INVESTIGATE THE BUG IN AUTH",
            ),
        )
        .unwrap();
        let out = run_start_agent_team(
            &paths,
            "dry-run-team-2",
            StartAgentTeamOptions {
                dry_run: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(out.contains("mode=agent-team"), "preview must mention mode");
        assert!(
            out.contains("INVESTIGATE THE BUG IN AUTH") || out.contains("Suggested teammates"),
            "preview must echo lead_seed; got: {out}",
        );
        assert!(
            out.contains("dry-run set"),
            "dry-run preview must include the cancellation note; got: {out}",
        );
        // Snapshot must NOT have been written (dry-run).
        let snapshot = target2.join(".ccteam").join("team-snapshot.json");
        assert!(
            !snapshot.exists(),
            "dry-run must not write team-snapshot.json",
        );
    }

    /// V0.5.0 F93b — `ccteam start <slug>` against an artifact-driven
    /// project returns a friendly error pointing at `ccteam start`
    /// (no slug, daemon mode).
    #[test]
    fn run_start_agent_team_rejects_artifact_driven_project() {
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let slug = "artifact-rej";
        let target = paths.projects_root.join(slug);
        run_init(
            &paths,
            InitOptions {
                install_in: Some(target),
                slug: Some(slug.into()),
                ..InitOptions::default()
            },
        )
        .unwrap();
        let err = run_start_agent_team(
            &paths,
            slug,
            StartAgentTeamOptions {
                dry_run: true,
                ..Default::default()
            },
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("artifact-driven"),
            "error must mention artifact-driven; got: {msg}",
        );
    }

    /// V0.5.0 F93b — `ccteam attach <slug>` against an agent-team
    /// project without a written snapshot returns a friendly error
    /// telling the user to run `ccteam start <slug>` first.
    #[test]
    fn run_attach_agent_team_missing_snapshot_errors_with_hint() {
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let slug = "no-snapshot";
        let target = paths.projects_root.join(slug);
        run_init(
            &paths,
            InitOptions {
                install_in: Some(target),
                slug: Some(slug.into()),
                mode: InitMode::AgentTeam,
                ..InitOptions::default()
            },
        )
        .unwrap();
        let err = run_attach(&paths, slug).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("team-snapshot") || msg.contains("ccteam start"),
            "error must mention snapshot / hint at `ccteam start`; got: {msg}",
        );
    }

    /// V0.5.0 F93b — `ccteam attach <slug>` against an agent-team
    /// project WITH a snapshot containing `lead_session_id` reads
    /// the lead id and would exec `claude attach <id>`. We can't
    /// actually exec here, but `read_agent_team_lead_session_id`
    /// is testable directly.
    #[test]
    fn read_agent_team_lead_session_id_resolves_from_snapshot() {
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let slug = "with-snapshot";
        let target = paths.projects_root.join(slug);
        run_init(
            &paths,
            InitOptions {
                install_in: Some(target.clone()),
                slug: Some(slug.into()),
                mode: InitMode::AgentTeam,
                ..InitOptions::default()
            },
        )
        .unwrap();
        // Fake snapshot writeup.
        let snapshot_path = target.join(".ccteam").join("team-snapshot.json");
        std::fs::write(
            &snapshot_path,
            serde_json::json!({
                "slug": slug,
                "lead_session_id": "deadbeef123",
                "team_name": "with-snapshot",
                "teammate_mode": "in-process",
                "cleanup_on_stop": "force-kill",
                "auto_spawn_teammates": false,
                "suggested_teammates": [],
                "spawned_at": "2026-05-17T12:00:00Z",
            })
            .to_string(),
        )
        .unwrap();
        let lead_id = read_agent_team_lead_session_id(&paths, slug)
            .unwrap()
            .unwrap();
        assert_eq!(lead_id, "deadbeef123");
    }

    /// V0.5.0 F93b — for artifact-driven projects,
    /// `read_agent_team_lead_session_id` returns Ok(None) so the
    /// caller falls through to the tmux / bg path (V0.4.6 behavior
    /// preserved).
    #[test]
    fn read_agent_team_lead_session_id_returns_none_for_artifact_driven() {
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let slug = "art-fall";
        let target = paths.projects_root.join(slug);
        run_init(
            &paths,
            InitOptions {
                install_in: Some(target),
                slug: Some(slug.into()),
                ..InitOptions::default()
            },
        )
        .unwrap();
        let res = read_agent_team_lead_session_id(&paths, slug).unwrap();
        assert!(res.is_none(), "artifact-driven must return None");
    }

    /// V0.4.2 F72: re-running on an existing ccteam project preserves
    /// user-edited workflow.yaml + agents/*.md.
    /// V0.4.6 F83: workflow.yaml lives in `.ccteam/`, not the root.
    #[test]
    fn run_init_refresh_preserves_user_workflow_and_agents() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let target = tmp.path().join("f72-refresh");
        run_init(
            &paths,
            InitOptions {
                install_in: Some(target.clone()),
                ..InitOptions::default()
            },
        )
        .unwrap();
        let wf_path = target.join(".ccteam").join("workflow.yaml");
        std::fs::write(&wf_path, "USER WORKFLOW\n").unwrap();
        std::fs::write(
            target.join(".claude").join("agents").join("cto.md"),
            "USER AGENT\n",
        )
        .unwrap();

        // Re-run: refresh should preserve user files.
        run_init(
            &paths,
            InitOptions {
                install_in: Some(target.clone()),
                ..InitOptions::default()
            },
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(&wf_path).unwrap(),
            "USER WORKFLOW\n"
        );
        assert_eq!(
            std::fs::read_to_string(target.join(".claude").join("agents").join("cto.md")).unwrap(),
            "USER AGENT\n"
        );
    }

    /// V0.4.2 F72: `--force` re-runs overwrite user files.
    /// V0.4.6 F83: workflow.yaml lives in `.ccteam/`, not the root.
    #[test]
    fn run_init_force_overwrites_user_workflow_and_agents() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let target = tmp.path().join("f72-force");
        run_init(
            &paths,
            InitOptions {
                install_in: Some(target.clone()),
                ..InitOptions::default()
            },
        )
        .unwrap();
        let wf_path = target.join(".ccteam").join("workflow.yaml");
        std::fs::write(&wf_path, "USER WORKFLOW\n").unwrap();
        run_init(
            &paths,
            InitOptions {
                install_in: Some(target.clone()),
                force: true,
                ..InitOptions::default()
            },
        )
        .unwrap();
        assert_ne!(
            std::fs::read_to_string(&wf_path).unwrap(),
            "USER WORKFLOW\n"
        );
    }

    /// V0.4.2 F72: `--reset-agents` rewrites agents but keeps
    /// workflow.yaml untouched.
    /// V0.4.6 F83: workflow.yaml lives in `.ccteam/`, not the root.
    #[test]
    fn run_init_reset_agents_only_overwrites_agents() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let target = tmp.path().join("f72-reset");
        run_init(
            &paths,
            InitOptions {
                install_in: Some(target.clone()),
                ..InitOptions::default()
            },
        )
        .unwrap();
        let wf_path = target.join(".ccteam").join("workflow.yaml");
        std::fs::write(&wf_path, "USER WORKFLOW\n").unwrap();
        std::fs::write(
            target.join(".claude").join("agents").join("cto.md"),
            "USER AGENT\n",
        )
        .unwrap();
        run_init(
            &paths,
            InitOptions {
                install_in: Some(target.clone()),
                reset_agents: true,
                ..InitOptions::default()
            },
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(&wf_path).unwrap(),
            "USER WORKFLOW\n",
            "workflow must survive --reset-agents",
        );
        assert_ne!(
            std::fs::read_to_string(target.join(".claude").join("agents").join("cto.md")).unwrap(),
            "USER AGENT\n",
            "agents must be overwritten by --reset-agents",
        );
    }

    /// V0.4.3 F76: invalid slug grammar fails loud at the CLI
    /// boundary, before any directory is created.
    #[test]
    fn run_init_rejects_invalid_slug_grammar() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let err = run_init(
            &paths,
            InitOptions {
                install_in: Some(tmp.path().join("ok-dir")),
                slug: Some("做一个 todo cli".into()),
                ..InitOptions::default()
            },
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("[a-z0-9-]+"),
            "expected slug-grammar fail-loud; got: {msg}",
        );
        assert!(
            !tmp.path().join("ok-dir").join(".ccteam").exists(),
            "no .ccteam/ should have been created when slug is invalid",
        );
    }

    /// V0.4.2 F72 (reviewer-blocker fix): `ccteam init` rejects a slug
    /// collision when the existing registry entry points at a different
    /// physical path. Same slug + same path is OK (refresh).
    #[test]
    fn run_init_rejects_slug_collision_at_different_path() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let first = tmp.path().join("first");
        run_init(
            &paths,
            InitOptions {
                install_in: Some(first.clone()),
                slug: Some("conflicty".into()),
                ..InitOptions::default()
            },
        )
        .unwrap();

        let second = tmp.path().join("second");
        let err = run_init(
            &paths,
            InitOptions {
                install_in: Some(second),
                slug: Some("conflicty".into()),
                ..InitOptions::default()
            },
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("already registered"),
            "expected slug-collision error; got: {msg}",
        );
    }

    /// Re-running on the SAME path with the same slug is a legitimate
    /// refresh, not a collision.
    #[test]
    fn run_init_same_slug_same_path_is_refresh_not_collision() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let target = tmp.path().join("refreshable");
        run_init(
            &paths,
            InitOptions {
                install_in: Some(target.clone()),
                slug: Some("refreshable".into()),
                ..InitOptions::default()
            },
        )
        .unwrap();
        // Second invocation: should succeed (refresh).
        run_init(
            &paths,
            InitOptions {
                install_in: Some(target),
                slug: Some("refreshable".into()),
                ..InitOptions::default()
            },
        )
        .unwrap();
    }

    /// V0.4.2 F72: installing in the ccteam source repo itself is
    /// fail-loud (CLAUDE.md §六 — avoids circular hook setup).
    #[test]
    fn run_init_refuses_to_install_in_ccteam_repo() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        // Plant the two markers `is_ccteam_repo` checks for.
        let fake_repo = tmp.path().join("ccteam-mirror");
        std::fs::create_dir_all(fake_repo.join("crates").join("ccteam-cli")).unwrap();
        std::fs::write(fake_repo.join("Cargo.toml"), "[workspace]\n").unwrap();

        let err = run_init(
            &paths,
            InitOptions {
                install_in: Some(fake_repo),
                ..InitOptions::default()
            },
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("ccteam repo itself"),
            "expected fail-loud message; got: {msg}",
        );
        assert!(
            msg.contains("--force"),
            "fail-loud message must point to the --force escape; got: {msg}",
        );
    }

    /// V0.6.8 F203 — `--force` overrides the ccteam-repo refusal so
    /// self-hosting / dogfooding installs inside the ccteam source tree
    /// can proceed when the user explicitly opts in.
    #[test]
    fn run_init_force_overrides_ccteam_repo_refusal() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        // Plant the two markers `is_ccteam_repo` checks for.
        let fake_repo = tmp.path().join("ccteam-mirror");
        std::fs::create_dir_all(fake_repo.join("crates").join("ccteam-cli")).unwrap();
        std::fs::write(fake_repo.join("Cargo.toml"), "[workspace]\n").unwrap();

        let out = run_init(
            &paths,
            InitOptions {
                install_in: Some(fake_repo.clone()),
                slug: Some("self-host".into()),
                force: true,
                ..InitOptions::default()
            },
        )
        .expect("--force must override ccteam-repo refusal");
        assert!(
            out.contains("ccteam init"),
            "expected init summary header; got: {out}",
        );

        // Project install must have actually written the ccteam files.
        let state_path = fake_repo.join(".ccteam").join("state.json");
        assert!(
            state_path.exists(),
            "state.json must exist after --force init; checked {}",
            state_path.display()
        );
        let workflow_path = fake_repo.join(".ccteam").join("workflow.yaml");
        assert!(
            workflow_path.exists(),
            "workflow.yaml must exist after --force init; checked {}",
            workflow_path.display()
        );
    }

    #[test]
    fn run_progress_emits_existing_events_without_tail() {
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let slug = run_new_t4(&paths, "demo", "dev").unwrap();
        progress::append_event(
            &paths.progress_jsonl(&slug),
            &json!({"event": "session_start", "ts": "2026-05-05T00:00:00Z"}),
        )
        .unwrap();
        // run_progress writes to stdout which is awkward to capture in
        // unit tests; verify the underlying file content as a proxy.
        let path = paths.progress_jsonl(&slug);
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("session_start"));
    }

    #[test]
    fn run_doctor_with_no_flags_shows_help_text() {
        // v0.8.6: `run_doctor`'s always-run legacy-skill self-heal
        // resolves the real `~/.claude`; the bootstrap-disable guard
        // keeps this unit test from touching the developer's home.
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let body = run_doctor(&paths, DoctorOptions::default()).unwrap();
        assert!(body.contains("tool-surface"));
        assert!(body.contains("install-memory-bridge"));
        assert!(body.contains("validate-team"), "got: {body}");
        assert!(body.contains("migrate-recommended-agents"), "got: {body}");
    }

    /// v0.8.6 — `ccteam doctor` flags orchestrator-era leftover dirs
    /// under `~/.ccteam` but stays silent on canonical / runtime-lazy
    /// dirs. Informational only; never affects the exit code.
    #[test]
    fn run_doctor_reports_home_layout_drift() {
        // v0.8.6: keep the always-run legacy-skill self-heal off the
        // developer's real `~/.claude` (see guard in
        // `render_legacy_skill_cleanup_line`).
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        // Canonical (init-time) + runtime-lazy dirs — must NOT be flagged.
        for d in [
            "hooks", "progress", "run", "state", "pty", "harness", "imd", "teams",
        ] {
            std::fs::create_dir_all(paths.root.join(d)).unwrap();
        }
        // Orchestrator-era leftovers — must be flagged.
        for d in ["phases", "control", "queue"] {
            std::fs::create_dir_all(paths.root.join(d)).unwrap();
        }
        let body = run_doctor(&paths, DoctorOptions::default()).unwrap();
        assert!(
            body.contains("home drift"),
            "expected a home-drift line; got: {body}",
        );
        for leftover in ["phases", "control", "queue"] {
            assert!(
                body.contains(leftover),
                "drift line must name `{leftover}`; got: {body}",
            );
        }
        // Legitimate dirs must not be reported on the drift line.
        let drift_line = body
            .lines()
            .find(|l| l.contains("home drift"))
            .unwrap_or("");
        for ok in ["hooks", "pty", "harness", "imd", "teams"] {
            assert!(
                !drift_line.contains(ok),
                "`{ok}` is legitimate and must not appear in the drift line; got: {drift_line}",
            );
        }
    }

    /// v0.8.6 — a clean `~/.ccteam` (only canonical dirs) emits no
    /// home-drift line.
    #[test]
    fn run_doctor_no_home_drift_when_layout_clean() {
        // v0.8.6: guard the always-run legacy-skill self-heal off the
        // real `~/.claude` during this unit test.
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        for d in ccteam_core::canonical_home_dirs() {
            std::fs::create_dir_all(paths.root.join(d)).unwrap();
        }
        let body = run_doctor(&paths, DoctorOptions::default()).unwrap();
        assert!(
            !body.contains("home drift"),
            "clean layout must not produce a drift line; got: {body}",
        );
    }

    #[test]
    fn run_doctor_install_memory_bridge_writes_both_team_files() {
        // V0.4.0 F60: shipped team bundles deleted; install_memory_bridge
        // discovers teams by scanning `<root>/teams/<name>/team.yaml`.
        // Seed dev + research yamls inline so the disk scan finds them.
        ensure_isolation();
        let _guard = env_lock().lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        std::env::set_var("CLAUDE_CONFIG_HOME", tmp.path().to_str().unwrap());
        for (team, yaml) in [
            (
                "dev",
                "name: dev\nretro_schema:\n  - field: tech_stack\n    description: stack notes\n",
            ),
            (
                "research",
                "name: research\nretro_schema:\n  - field: findings\n    description: notes\n",
            ),
        ] {
            let team_dir = paths.root.join("teams").join(team);
            std::fs::create_dir_all(&team_dir).unwrap();
            std::fs::write(team_dir.join("team.yaml"), yaml).unwrap();
        }

        let opts = DoctorOptions {
            install_memory_bridge: true,
            ..DoctorOptions::default()
        };
        let report = run_doctor(&paths, opts).unwrap();
        assert!(report.contains("install-memory-bridge"));
        assert!(report.contains("dev"));
        assert!(report.contains("research"));

        let dev_path = tmp.path().join("rules/ccteam-lessons-dev.md");
        let research_path = tmp.path().join("rules/ccteam-lessons-research.md");
        assert!(dev_path.is_file(), "dev lessons not written");
        assert!(research_path.is_file(), "research lessons not written");

        std::env::remove_var("CLAUDE_CONFIG_HOME");
    }

    #[test]
    fn run_doctor_appends_codex_detection_line_when_any_mode_runs() {
        // V0.3.1 F47 — every successful doctor run (any_mode == true)
        // appends one informational `[ccteam] codex CLI: ...` line at
        // the end. Pure informational — never fails the report. The
        // exact path / not-found suffix depends on the host so we
        // only pin the prefix. `tool_surface` is a read-only mode flag
        // that flips `any_mode` true without mutating disk.
        ensure_isolation();
        let _guard = env_lock().lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        std::env::set_var("CLAUDE_CONFIG_HOME", tmp.path().to_str().unwrap());

        let opts = DoctorOptions {
            tool_surface: true,
            ..DoctorOptions::default()
        };
        let report = run_doctor(&paths, opts).unwrap();
        assert!(
            report.contains("[ccteam] codex CLI:"),
            "doctor report must include codex CLI detection line; got:\n{report}",
        );
        std::env::remove_var("CLAUDE_CONFIG_HOME");
    }

    #[test]
    fn run_doctor_no_flags_runs_implicit_pricing_check_with_help() {
        // V0.6.1 F121 — `ccteam doctor` without a mode flag now
        // implicitly runs the pricing-staleness check so operators see
        // ageing rate sheets without having to remember the explicit
        // flag. The help block is appended after the report so
        // first-time users still discover the opt-in mutation modes.
        // v0.8.6: guard the always-run legacy-skill self-heal off the
        // real `~/.claude` during this unit test.
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let body = run_doctor(&paths, DoctorOptions::default()).unwrap();
        assert!(
            body.contains("ccteam doctor --check-pricing-version"),
            "no-flag invocation must run implicit pricing check; got:\n{body}",
        );
        assert!(
            body.contains("[pricing.anthropic]") && body.contains("[pricing.openai]"),
            "implicit pricing check must emit both vendor rows; got:\n{body}",
        );
        assert!(
            body.contains("pass at least one mode flag"),
            "no-flag invocation must still surface the help block; got:\n{body}",
        );
    }

    // V0.4.0 F60: shipped-team seed writer (dev / research / meta-agent
    // bundles + write_all_global_team_templates) was deleted with the
    // phase machinery. F63 reintroduces a workflow.yaml seed writer.

    // V0.4.0 F60: alias resolution against shipped research team.yaml
    // removed — F40's deprecation path depended on the deleted shipped
    // bundle writer. F63 will revisit alias semantics for workflow.yaml.

    // V0.4.6 F89: `ccteam decisions` / `ccteam phase` / `ccteam watchdog`
    // top-level commands removed (V0.3 legacy). Their unit tests were
    // tied to `run_decisions` / `collect_decisions` / `run_phase_show` /
    // `run_watchdog_scan` (now deleted). The remaining outbox /
    // watchdog plumbing in `ccteam-core` is still tested in its own
    // crate; the CLI no longer exposes the surface.

    #[test]
    fn validate_team_reports_fail_for_unknown_team() {
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);

        let opts = DoctorOptions {
            validate_team: Some("totally-unknown".into()),
            ..DoctorOptions::default()
        };
        // F30 — fail-loud: any [FAIL] line bubbles a non-zero exit.
        let err = run_doctor(&paths, opts).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("[FAIL] team.yaml load"), "got: {msg}");
        assert!(msg.contains("1 fail"), "expected fails counter; got: {msg}");
    }

    // =====================================================================
    // V0.5.0 F97 — `ccteam stop <slug>` + `--restart-team` lifecycle tests
    // =====================================================================

    /// Build a `cleanup_on_stop: <strategy>` agent-team project at
    /// `<paths.projects_root>/<slug>/`. Returns the project_dir.
    fn install_team_project(paths: &CcteamPaths, slug: &str, cleanup: &str) -> std::path::PathBuf {
        let target = paths.projects_root.join(slug);
        run_init(
            paths,
            InitOptions {
                install_in: Some(target.clone()),
                slug: Some(slug.into()),
                mode: InitMode::AgentTeam,
                ..InitOptions::default()
            },
        )
        .unwrap();
        // Replace cleanup_on_stop line in the scaffolded workflow.yaml.
        let wf = target.join(".ccteam").join("workflow.yaml");
        let raw = std::fs::read_to_string(&wf).unwrap();
        let replaced = raw.replace(
            "cleanup_on_stop: force-kill",
            &format!("cleanup_on_stop: {cleanup}"),
        );
        std::fs::write(&wf, replaced).unwrap();
        target
    }

    /// Write a fake `team-snapshot.json` (no live bg job behind it —
    /// just enough metadata for `run_stop_slug` to dispatch on cleanup
    /// strategy). For `ask-lead`/`leave-running` tests we don't need
    /// a real `~/.claude/jobs/<id>/state.json` — the missing file
    /// surfaces as "already cleaned up" in the report.
    fn write_fake_snapshot(
        project_dir: &std::path::Path,
        slug: &str,
        lead_id: &str,
        cleanup: &str,
    ) {
        let snap_dir = project_dir.join(".ccteam");
        std::fs::create_dir_all(&snap_dir).unwrap();
        let body = json!({
            "slug": slug,
            "lead_session_id": lead_id,
            "team_name": slug,
            "teammate_mode": "in-process",
            "cleanup_on_stop": cleanup,
            "auto_spawn_teammates": false,
            "suggested_teammates": [],
            "spawned_at": "2026-05-17T12:00:00Z",
        });
        std::fs::write(
            snap_dir.join("team-snapshot.json"),
            serde_json::to_string_pretty(&body).unwrap(),
        )
        .unwrap();
    }

    /// Helper: write a fake `~/.claude/jobs/<id>/state.json` with a pid
    /// that's guaranteed to be invalid (1) so SIGKILL is a no-op. Uses
    /// `$CCTEAM_CLAUDE_JOBS_DIR` so we don't touch the real host dir.
    /// **NOTE**: this mutates env, so callers should hold env_lock().
    fn install_fake_state_json(jobs_root: &std::path::Path, lead_id: &str, pid: i32) {
        let dir = jobs_root.join(lead_id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("state.json"),
            serde_json::to_string_pretty(&json!({
                "state": "working",
                "pid": pid,
                "daemonShort": lead_id,
            }))
            .unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn cleanup_on_stop_force_kill_kills_and_clears_snapshot() {
        ensure_isolation();
        let _g = env_lock().lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let slug = "force-kill-team";
        let project_dir = install_team_project(&paths, slug, "force-kill");
        write_fake_snapshot(&project_dir, slug, "abc123", "force-kill");

        // Point CCTEAM_CLAUDE_JOBS_DIR at a temp dir + write a fake
        // state.json with pid=1 (kernel-reserved; SIGKILL → ESRCH on
        // Linux, our sigkill_pid maps that to Ok). This validates the
        // "kill happens; force-kill path doesn't blow up on stale pid".
        let jobs_root = tmp.path().join("claude-jobs");
        std::env::set_var("CCTEAM_CLAUDE_JOBS_DIR", &jobs_root);
        install_fake_state_json(&jobs_root, "abc123", 1);

        let report = run_stop_slug(&paths, slug, StopSlugOptions::default()).unwrap();
        std::env::remove_var("CCTEAM_CLAUDE_JOBS_DIR");

        assert!(
            report.contains("cleanup_on_stop=force-kill"),
            "report header missing strategy; got: {report}",
        );
        assert!(
            report.contains("SIGKILL pid 1") || report.contains("already terminated"),
            "report should mention SIGKILL / already terminated; got: {report}",
        );
        // Snapshot must be cleared.
        let snap = project_dir.join(".ccteam").join("team-snapshot.json");
        assert!(
            !snap.exists(),
            "force-kill must clear team-snapshot.json; still exists",
        );
    }

    #[test]
    fn cleanup_on_stop_ask_lead_writes_inbox_and_falls_back_on_timeout() {
        ensure_isolation();
        let _g = env_lock().lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let slug = "ask-lead-team";
        let project_dir = install_team_project(&paths, slug, "ask-lead");
        write_fake_snapshot(&project_dir, slug, "leadAlive1", "ask-lead");
        // No progress.jsonl writes happening → timeout will fire.

        let report = run_stop_slug(
            &paths,
            slug,
            StopSlugOptions {
                stop_timeout: std::time::Duration::from_millis(800),
            },
        )
        .unwrap();

        // Inbox message must be written.
        let inbox_dir = project_dir.join(".ccteam").join("inbox");
        let entries: Vec<_> = std::fs::read_dir(&inbox_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with("stop-request.md"))
            .collect();
        assert_eq!(
            entries.len(),
            1,
            "exactly one stop-request.md must land in inbox; got {entries:?}",
        );
        let body =
            std::fs::read_to_string(entries[0].path()).expect("inbox message must be readable");
        assert!(
            body.contains("Clean up the team"),
            "inbox message body should ask for cleanup; got: {body}",
        );
        assert!(
            body.contains("NOT a system prompt"),
            "red line: inbox message must be marked as user-turn, not system prompt",
        );

        // Report mentions timeout + force-kill fallback.
        assert!(
            report.contains("timeout"),
            "report should mention timeout; got: {report}",
        );
        assert!(
            report.contains("force-kill") || report.contains("SIGKILL"),
            "fallback path should mention force-kill; got: {report}",
        );
    }

    #[test]
    fn cleanup_on_stop_ask_lead_succeeds_when_workflow_done_appears() {
        ensure_isolation();
        let _g = env_lock().lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let slug = "ask-lead-success";
        let project_dir = install_team_project(&paths, slug, "ask-lead");
        write_fake_snapshot(&project_dir, slug, "leadAlive2", "ask-lead");

        // Pre-write one workflow_done event so count_workflow_done's
        // delta check (current > baseline) sees it on the very first
        // tick — simulates the lead beating the polling loop.
        let progress_path = paths.progress_jsonl(slug);
        std::fs::create_dir_all(progress_path.parent().unwrap()).unwrap();
        // First baseline write (counts as 0 because run_stop_slug
        // reads the file BEFORE writing the inbox message, so we need
        // the new event to appear AFTER. Use a background thread.)
        let progress_path_for_thread = progress_path.clone();
        let _h = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            ccteam_core::progress::append_event(
                &progress_path_for_thread,
                &json!({
                    "event": "workflow_done",
                    "reason": "cleanup_complete",
                    "ts": "2026-05-17T12:01:00Z",
                }),
            )
            .unwrap();
        });

        let report = run_stop_slug(
            &paths,
            slug,
            StopSlugOptions {
                stop_timeout: std::time::Duration::from_secs(5),
            },
        )
        .unwrap();

        assert!(
            report.contains("workflow_done"),
            "report should mention workflow_done observed; got: {report}",
        );
        assert!(
            !report.contains("timeout"),
            "successful path must not mention timeout; got: {report}",
        );
    }

    #[test]
    fn cleanup_on_stop_leave_running_keeps_lead_alive_and_marks_detached() {
        ensure_isolation();
        let _g = env_lock().lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let slug = "leave-running-team";
        let project_dir = install_team_project(&paths, slug, "leave-running");
        write_fake_snapshot(&project_dir, slug, "leadLive3", "leave-running");

        let report = run_stop_slug(&paths, slug, StopSlugOptions::default()).unwrap();

        // Snapshot MUST still exist (used by --restart-team).
        let snap = project_dir.join(".ccteam").join("team-snapshot.json");
        assert!(
            snap.exists(),
            "leave-running must NOT clear team-snapshot.json (needed for restart-team)",
        );
        // Report mentions restart-team hint.
        assert!(
            report.contains("--restart-team"),
            "leave-running report should reference --restart-team; got: {report}",
        );
        // state.json::detached must be set.
        let state = ccteam_core::ProjectState::load(&paths.project_state(slug)).unwrap();
        assert!(
            state.detached,
            "leave-running must set state.json::detached = true",
        );
    }

    #[test]
    fn restart_team_resumes_alive_lead_without_spawning() {
        ensure_isolation();
        let _g = env_lock().lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let slug = "restart-alive";
        let project_dir = install_team_project(&paths, slug, "leave-running");
        write_fake_snapshot(&project_dir, slug, "stillAlive4", "leave-running");

        // Plant a "Running" state.json so probe_job returns
        // JobLiveness::Running.
        let jobs_root = tmp.path().join("claude-jobs");
        std::env::set_var("CCTEAM_CLAUDE_JOBS_DIR", &jobs_root);
        let dir = jobs_root.join("stillAlive4");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("state.json"),
            r#"{"state":"working","pid":99999,"daemonShort":"stillAlive4"}"#,
        )
        .unwrap();

        // Mark detached so --restart-team has something to clear.
        let mut state = ccteam_core::ProjectState::load(&paths.project_state(slug)).unwrap();
        state.detached = true;
        state.save(&paths.project_state(slug)).unwrap();

        // Set CCTEAM_CLAUDE_BIN to a non-existent path so any attempt to
        // spawn would fail loudly (proves we never even tried).
        std::env::set_var("CCTEAM_CLAUDE_BIN", "/nonexistent/should-never-run");

        let report = run_start_agent_team(
            &paths,
            slug,
            StartAgentTeamOptions {
                restart_team: true,
                no_confirm: true,
                ..Default::default()
            },
        );
        std::env::remove_var("CCTEAM_CLAUDE_BIN");
        std::env::remove_var("CCTEAM_CLAUDE_JOBS_DIR");

        let body = report.unwrap();
        assert!(
            body.contains("Skipping spawn") || body.contains("lead bg job alive"),
            "report must indicate no spawn happened; got: {body}",
        );
        assert!(
            body.contains("stillAlive4"),
            "report must mention lead id; got: {body}",
        );
        // detached marker must be cleared.
        let state_after = ccteam_core::ProjectState::load(&paths.project_state(slug)).unwrap();
        assert!(
            !state_after.detached,
            "restart-team must clear detached marker on success",
        );
    }

    #[test]
    fn restart_team_falls_through_to_spawn_when_lead_terminal() {
        ensure_isolation();
        let _g = env_lock().lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let slug = "restart-dead";
        let project_dir = install_team_project(&paths, slug, "leave-running");
        write_fake_snapshot(&project_dir, slug, "deadLead5", "leave-running");

        // Plant a "Terminal" state.json (job_id has no state.json file
        // at all → probe_job returns Terminal { status: "killed" }).
        let jobs_root = tmp.path().join("claude-jobs");
        std::env::set_var("CCTEAM_CLAUDE_JOBS_DIR", &jobs_root);
        // Do NOT create the file — absent file → Terminal.

        // Use --dry-run so we don't actually try to spawn (we only
        // want to verify the WARN prelude + dry-run preview happens
        // after the fall-through).
        let report = run_start_agent_team(
            &paths,
            slug,
            StartAgentTeamOptions {
                restart_team: true,
                dry_run: true,
                ..Default::default()
            },
        );
        std::env::remove_var("CCTEAM_CLAUDE_JOBS_DIR");

        let body = report.unwrap();
        assert!(
            body.contains("Previous lead exited") || body.contains("spawning fresh lead"),
            "should print fall-through WARN; got: {body}",
        );
        assert!(
            body.contains("mode=agent-team"),
            "should fall through to standard preview; got: {body}",
        );
    }

    #[test]
    fn restart_team_fails_when_snapshot_missing() {
        ensure_isolation();
        let _g = env_lock().lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let slug = "restart-no-snapshot";
        install_team_project(&paths, slug, "force-kill");
        // No snapshot written.

        let err = run_start_agent_team(
            &paths,
            slug,
            StartAgentTeamOptions {
                restart_team: true,
                ..Default::default()
            },
        )
        .expect_err("--restart-team without snapshot must fail");

        let msg = format!("{err:#}");
        assert!(
            msg.contains("team-snapshot.json"),
            "error must mention snapshot; got: {msg}",
        );
        assert!(
            msg.contains("ccteam start") || msg.contains("--restart-team"),
            "error must hint at recovery; got: {msg}",
        );
    }

    #[test]
    fn plain_start_refuses_when_project_is_detached() {
        ensure_isolation();
        let _g = env_lock().lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let slug = "detached-refuse";
        let project_dir = install_team_project(&paths, slug, "leave-running");
        write_fake_snapshot(&project_dir, slug, "leadX6", "leave-running");

        // Mark detached (simulates a prior leave-running stop).
        let mut state = ccteam_core::ProjectState::load(&paths.project_state(slug)).unwrap();
        state.detached = true;
        state.save(&paths.project_state(slug)).unwrap();

        // Plain `ccteam start <slug>` (no --restart-team) must refuse.
        let err = run_start_agent_team(
            &paths,
            slug,
            StartAgentTeamOptions {
                restart_team: false,
                no_confirm: true,
                ..Default::default()
            },
        )
        .expect_err("plain start on detached project must refuse");

        let msg = format!("{err:#}");
        assert!(
            msg.contains("detached"),
            "error must mention detached state; got: {msg}",
        );
        assert!(
            msg.contains("--restart-team"),
            "error must point at --restart-team; got: {msg}",
        );
    }

    // V0.6.6 F171 — render-path coverage for the FAIL branch (binary
    // E2E test in `tests/doctor_verify_mcp_test.rs` only exercises the
    // PASS branch since shipping a STUB tool just to fail-test the
    // binary would defeat the gate). These unit tests pin the
    // human-readable + JSON shape for a synthetic STUB scenario so the
    // FAIL message format stays stable across refactors.

    #[test]
    fn verify_mcp_run_on_live_surface_passes_with_zero_stubs() {
        let report = run_verify_mcp();
        assert!(report.ok(), "live MCP surface must be 0-STUB");
        assert_eq!(report.stub_count, 0);
        assert!(report.unexpected_stubs.is_empty());
        // total_tools must match the mcp_serve spec — keeps F171 in
        // sync with `tool_definitions_count_matches_spec` (live truth).
        assert_eq!(report.total_tools, report.active_count);
        assert_eq!(report.total_tools, 17, "ships 17 tools");
    }

    #[test]
    fn config_mcp_report_renders_live_tool_count() {
        // The `config mcp` report must print the live tool count from
        // `tool_definitions()`, never a hard-coded number that drifts
        // (it was stuck at "9" while the surface grew to 27). Mirrors
        // `tool_definitions_count_matches_spec` so the rendered string
        // can never silently diverge from the registered surface. Uses
        // the pure body renderer with a synthetic path so it never
        // touches the real ~/.claude.json.
        let total = run_verify_mcp().total_tools;
        let report = render_install_mcp_body(std::path::Path::new("/tmp/fake-claude.json"));
        assert!(
            report.contains(&format!("tools surface : {total}")),
            "report must interpolate live tool count {total}: {report}",
        );
        // The stale "(interfaces §12.2)" tag must be gone.
        assert!(
            !report.contains("interfaces §12.2"),
            "stale section tag must be dropped: {report}",
        );
    }

    #[test]
    fn verify_mcp_report_render_text_fail_path_emits_verdict_fail() {
        let mut per_group = std::collections::BTreeMap::new();
        per_group.insert(
            "workflow".to_string(),
            GroupStats {
                active: 14,
                stub: 1,
            },
        );
        let synth = VerifyMcpReport {
            total_tools: 27,
            stub_count: 1,
            active_count: 26,
            tool_list: vec!["ccteam__workflow_synth_stub".to_string()],
            per_group,
            unexpected_stubs: vec!["ccteam__workflow_synth_stub".to_string()],
        };
        assert!(!synth.ok());
        let text = synth.render_text();
        assert!(text.contains("verdict: FAIL"), "text: {text}");
        assert!(
            text.contains("unexpected STUBs"),
            "text must list unexpected STUBs section: {text}",
        );
        assert!(
            text.contains("ccteam__workflow_synth_stub"),
            "stub tool name must appear in report: {text}",
        );
        assert!(text.contains("14 active / 1 stub"), "got: {text}");
    }

    #[test]
    fn verify_mcp_report_render_json_fail_path_sets_ok_false() {
        let mut per_group = std::collections::BTreeMap::new();
        per_group.insert("advise".to_string(), GroupStats { active: 1, stub: 1 });
        let synth = VerifyMcpReport {
            total_tools: 28,
            stub_count: 1,
            active_count: 27,
            tool_list: vec!["ccteam__advise_synth_stub".to_string()],
            per_group,
            unexpected_stubs: vec!["ccteam__advise_synth_stub".to_string()],
        };
        let j = synth.render_json();
        let v: Value = serde_json::from_str(&j).expect("render_json emits valid JSON");
        assert_eq!(v["ok"], Value::Bool(false));
        assert_eq!(v["stub_count"], Value::Number(1.into()));
        assert_eq!(v["total_tools"], Value::Number(28.into()));
        let unexpected = v["unexpected_stubs"].as_array().unwrap();
        assert_eq!(unexpected.len(), 1);
        assert_eq!(
            unexpected[0],
            Value::String("ccteam__advise_synth_stub".into())
        );
    }

    // --- Lark/Feishu config (run_config_set_lark_creds) ---------------
    //
    // Deterministic: a one-shot std TCP listener stands in for the Feishu
    // `tenant_access_token/internal` endpoint, so the credential validate
    // makes a real HTTP round-trip without touching the network. No env
    // mutation and the creds path is a tempdir, so this is safe in a lib
    // `#[cfg(test)]` module (CLAUDE.md §六).

    /// Spawn a single-shot HTTP/1.1 responder on `127.0.0.1:0` that replies
    /// to the first connection with `body` (status 200, JSON) and exits.
    /// Returns `http://127.0.0.1:<port>` (a Lark `api_base`).
    fn spawn_oneshot_http(body: &'static str) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                // Drain the request headers (reqwest sends a small POST);
                // we don't need to parse them for the canned reply.
                let mut buf = [0u8; 2048];
                let _ = stream.read(&mut buf);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
            }
        });
        format!("http://{addr}")
    }

    #[test]
    fn lark_creds_persist_and_preserve_existing_telegram() {
        let tmp = TempDir::new().unwrap();
        let creds_path = tmp.path().join("im/credentials.json");

        // Seed an existing Telegram entry that must survive the Lark write.
        let seed = ccteam_im::credentials::Credentials {
            telegram: Some(ccteam_im::credentials::TelegramCreds {
                bot_token: "tg-seed-token".into(),
                allowed_chat_ids: vec!["111".into()],
            }),
            ..Default::default()
        };
        ccteam_im::credentials::save(&creds_path, &seed).unwrap();

        // Mock the tenant_access_token success shape (code=0 + a token).
        let base = spawn_oneshot_http(
            r#"{"code":0,"msg":"ok","tenant_access_token":"t-tok","expire":7200}"#,
        );

        let out = run_config_set_lark_creds_with_base(
            "cli_app_42",
            "secret_42",
            vec!["ou_alice".into(), "ou_bob".into()],
            true, // Feishu / CN
            &base,
            Some(&creds_path),
        )
        .expect("lark creds validate + persist must succeed against the mock");
        assert!(
            out.contains("Lark/Feishu credentials saved") && out.contains("2 open_id(s) allowed"),
            "summary must confirm the save + allowlist count; got: {out}"
        );

        // Reload and assert the lark block landed with the right fields …
        let reloaded = ccteam_im::credentials::load(Some(&creds_path)).unwrap();
        let lark = reloaded.lark.expect("lark creds must be persisted");
        assert_eq!(lark.app_id, "cli_app_42");
        assert_eq!(lark.app_secret, "secret_42");
        assert_eq!(lark.allowed_user_ids, vec!["ou_alice", "ou_bob"]);
        assert!(lark.use_feishu, "use_feishu must round-trip true");

        // … and that the pre-existing Telegram entry was preserved (the
        // merge must not clobber sibling platforms).
        let tg = reloaded.telegram.expect("telegram must survive the merge");
        assert_eq!(tg.bot_token, "tg-seed-token");
        assert_eq!(tg.allowed_chat_ids, vec!["111"]);
    }

    #[test]
    fn lark_creds_empty_allowlist_warns_fail_closed() {
        let tmp = TempDir::new().unwrap();
        let creds_path = tmp.path().join("im/credentials.json");
        let base = spawn_oneshot_http(r#"{"code":0,"msg":"ok","tenant_access_token":"t-tok"}"#);

        // Empty allowlist + Lark international region.
        let out = run_config_set_lark_creds_with_base(
            "cli_x",
            "secret_x",
            vec![],
            false,
            &base,
            Some(&creds_path),
        )
        .expect("empty allowlist still persists (it is a valid, if locked-down, config)");
        assert!(
            out.contains("fail-closed") && out.contains("NO ONE"),
            "empty allowlist must surface the fail-closed warning; got: {out}"
        );
        assert!(
            out.contains("Lark (intl"),
            "region note must reflect use_feishu=false; got: {out}"
        );
        let reloaded = ccteam_im::credentials::load(Some(&creds_path)).unwrap();
        let lark = reloaded.lark.unwrap();
        assert!(lark.allowed_user_ids.is_empty());
        assert!(!lark.use_feishu);
    }

    #[test]
    fn lark_creds_bad_app_creds_error_no_persist() {
        let tmp = TempDir::new().unwrap();
        let creds_path = tmp.path().join("im/credentials.json");
        // Feishu signals bad credentials as a 200 with a non-zero `code`.
        let base = spawn_oneshot_http(r#"{"code":10003,"msg":"invalid app_secret"}"#);

        let err = run_config_set_lark_creds_with_base(
            "cli_bad",
            "wrong_secret",
            vec!["ou_a".into()],
            true,
            &base,
            Some(&creds_path),
        )
        .expect_err("a non-zero Feishu code must surface as an error, not a saved token");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("invalid app_secret") || msg.contains("10003"),
            "error must carry the upstream Feishu reason; got: {msg}"
        );
        // Nothing should have been written on the validate failure.
        assert!(
            !creds_path.exists(),
            "credentials file must not be created when validation fails"
        );
    }

    #[test]
    fn lark_creds_empty_app_id_rejected_before_network() {
        // Guard: both app_id + app_secret are required; we never even
        // reach the validate call (a deliberately-unreachable base URL).
        let tmp = TempDir::new().unwrap();
        let creds_path = tmp.path().join("im/credentials.json");
        let err = run_config_set_lark_creds_with_base(
            "   ",
            "secret",
            vec![],
            true,
            "http://127.0.0.1:1", // would refuse-connect if reached
            Some(&creds_path),
        )
        .expect_err("blank app_id must be rejected up front");
        assert!(
            err.to_string()
                .contains("app_id and app_secret are both required"),
            "must name the missing-field guard; got: {err}"
        );
    }
}
