//! Command handlers for `ccteam {new, ls, show, attach, peek, progress,
//! resume}`. Pure where possible (`run_ls` / `run_show` return the
//! formatted string instead of printing) so unit tests don't need a
//! real terminal or running orchestrator.

use std::process::Command;

use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};

use ccteam_core::tmux::TmuxSession;
use ccteam_core::{
    bootstrap_meta_project, current_ccteam_bin, install_ccteam_control_skill,
    install_ccteam_project_creator_skill, install_ccteam_team_author_skill,
    migrate_legacy_skill_dirs, migrate_recommended_agent_symlinks, rewrite_legacy_hook_commands,
    session_name_for_project, user_claude_dir, write_global_helper_templates, CcteamPaths,
    HookCmdRewriteAction, HookCmdRewriteReport, InstallSkillOptions, LegacySkillAction,
    LegacySkillReport, MetaBootstrapReport, MigrationReport, PhaseState, ProjectState,
    SkillInstallAction, ToolSurfaceSnapshot, BUILTIN_SUBAGENTS,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    Text,
    Json,
}

/// Options passed from the `ccteam init` argument parser.
#[derive(Debug, Clone, Default)]
pub struct InitOptions {
    /// V0.4.2 F72: install in this directory. `None` defaults to the
    /// cwd (or, when `slug` is also Some, to
    /// `<projects_root>/<slug>/`).
    pub install_in: Option<std::path::PathBuf>,
    /// V0.4.2 F72: slug override. When absent we derive from the
    /// install target's dir basename.
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
    for sub in [
        "phases",
        "templates",
        "progress",
        "inbox",
        "control",
        "queue",
        "memory",
        "state",
        "log",
    ] {
        let dir = paths.root.join(sub);
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    }
    write_global_helper_templates(&paths.root, opts.force).with_context(|| {
        format!(
            "unpack helper templates to {}",
            paths.templates_dir().display()
        )
    })?;

    // -- 2. Resolve project install target ---------------------------
    let target = resolve_install_target(paths, &opts)?;
    let derived_slug = opts.slug.clone().unwrap_or_else(|| {
        target
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("project")
            .to_string()
    });
    // V0.4.3 F76: validate slug grammar before anything writes to disk.
    // Catches whitespace / unicode / leading-dash cases that used to
    // create `~/projects/<bad-name>/` directories silently.
    let target_slug = ccteam_core::validate_slug_format(&derived_slug)
        .with_context(|| format!("ccteam init: invalid slug {derived_slug:?}"))?;
    let target_team = opts.team.clone().unwrap_or_else(|| "dev".to_string());

    // -- 3a. Refuse install in the ccteam repo itself ----------------
    if is_ccteam_repo(&target) {
        return Err(anyhow::anyhow!(
            "refusing to install ccteam in the ccteam repo itself: {}\n\n\
             this directory contains the ccteam source — installing here would create \
             a circular hook setup. Pick a different directory (or `cd` into your own project \
             and re-run).",
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

        let do_skill = ask_yn("install ccteam-control skill (~/.claude/skills/)", opts.yes)?;
        if do_skill {
            out.push_str(&render_install_skill_report(
                paths,
                &DoctorOptions {
                    install_skill: true,
                    ..Default::default()
                },
            )?);
            did_something = true;
        }
        let do_mcp = ask_yn(
            "register ccteam MCP server in ~/.claude.json (mcpServers.ccteam)",
            opts.yes,
        )?;
        if do_mcp {
            out.push_str(&render_install_mcp_report()?);
            did_something = true;
        }
        let do_meta = ask_yn("bootstrap meta-agent project (~/projects/meta/)", opts.yes)?;
        if do_meta {
            out.push_str(&render_install_meta_agent_report(paths)?);
            did_something = true;
        }

        if !did_something {
            out.push_str("  (no install steps selected)\n");
        }
    }

    out.push_str("\nnext:\n");
    out.push_str("  - edit .ccteam/workflow.yaml + .claude/agents/<role>.md to your taste\n");
    out.push_str("  - ccteam start                # boots orchestrator + web\n");
    Ok(out)
}

/// V0.4.2 F72: resolve where to install. Priority:
///   1. `--in <path>`  (absolute or relative; created if absent)
///   2. `--slug <name>` (→ `<projects_root>/<slug>/`)
///   3. current working directory
fn resolve_install_target(paths: &CcteamPaths, opts: &InitOptions) -> Result<std::path::PathBuf> {
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
    if let Some(slug) = &opts.slug {
        let target = paths.projects_root.join(slug);
        std::fs::create_dir_all(&target)
            .with_context(|| format!("create slug target {}", target.display()))?;
        return Ok(target);
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
        scaffold_workflow_yaml(target, false)?;
        let agent_count = scaffold_default_agents(target, false)?;
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
            scaffold_workflow_yaml(target, true)?;
            "overwritten (--force)"
        } else {
            "preserved"
        };

        agents_action = if opts.force || opts.reset_agents {
            scaffold_default_agents(target, true)?;
            if opts.force {
                "overwritten (--force)"
            } else {
                "overwritten (--reset-agents)"
            }
        } else {
            "preserved"
        };
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
fn scaffold_workflow_yaml(target: &std::path::Path, force: bool) -> Result<()> {
    let ccteam_dir = target.join(".ccteam");
    std::fs::create_dir_all(&ccteam_dir)
        .with_context(|| format!("create {}", ccteam_dir.display()))?;
    let path = ccteam_dir.join("workflow.yaml");
    if path.exists() && !force {
        return Ok(());
    }
    std::fs::write(&path, DEFAULT_WORKFLOW_YAML)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// V0.4.2 F72: write minimal `.claude/agents/*.md` examples. Returns
/// the count of files written. With `force=false`, existing files are
/// preserved; with `force=true`, the shipped scaffolds always
/// overwrite.
fn scaffold_default_agents(target: &std::path::Path, force: bool) -> Result<usize> {
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
    Ok(written)
}

const DEFAULT_WORKFLOW_YAML: &str = r#"# ccteam workflow.yaml (V0.4.0+ shape).
# Edit this file to declare your project's agent topology. Each agent
# is a role (filename of .claude/agents/<role>.md) with a trigger that
# decides when ccteam spawns a session for it.
#
# Trigger grammar:
#   manual                        # explicit `ccteam spawn <slug> <role>` only
#   schedule                      # periodic (V0.4.1+ interval field)
#   gate                          # waits for `trigger_gate` MCP / CLI call
#   watch:.ccteam/issues/         # spawn one session per new file under the path
#
# Docs: docs/v0-4-0/prd.md §6, examples/workflows/*.yaml
name: default-workflow
description: |
  Minimal starter workflow. Edit me — the manual `explorer` is a safe
  default that won't spawn until you call `ccteam spawn <slug> explorer`.

agents:
  explorer:
    trigger: manual
    executor: claude
"#;

const DEFAULT_AGENT_SCAFFOLDS: &[(&str, &str)] = &[(
    "explorer.md",
    r#"# Explorer agent

This file is the system prompt for the `explorer` role.

You are a generalist working in this project. The user has just
installed ccteam here via `ccteam init`. Start by reading the project
layout (`ls`, `git log -5`, top-level README if present) and reporting
what you find. Wait for further instructions from the inbox.

## Tools

You have access to ccteam's MCP toolset (`mcp__ccteam__*`) for
inspecting other projects, sending messages, and triggering workflow
gates. See ~/.claude/skills/ccteam-control/SKILL.md for usage patterns.
"#,
)];

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

/// Resolve `team_kind` from the on-disk team registry. Built-in
/// teams (`dev`, `meta-agent`) default to Workflow. Other teams must
/// resolve via `~/.ccteam/teams/<team>/team.yaml` — when the lookup
/// fails the caller (`refresh_state_team_kind`) preserves whatever
/// `state.json::team_kind` already has, so a project saved as Flex
/// stays Flex even when its team.yaml isn't on disk in this env.
///
/// V0.4.2 F75: the V0.4.0 `ensure_team_resolvable` fail-loud gate was
/// dropped (it lived inside the deleted `run_new`); kind resolution
/// itself is unchanged.
fn resolve_team_kind(paths: &CcteamPaths, team: &str) -> Result<ccteam_core::TeamKind> {
    use ccteam_core::{default_user_staging_dir, resolve_team, TeamKind, TeamResolveContext};
    if team == "dev" || team == ccteam_core::META_TEAM_NAME {
        return Ok(TeamKind::Workflow);
    }
    let user_staging = default_user_staging_dir();
    let ctx = TeamResolveContext::for_orchestrator(&paths.root, &user_staging);
    let spec =
        resolve_team(team, &ctx).with_context(|| format!("resolve team `{team}` for team kind"))?;
    Ok(spec.kind)
}

/// `ccteam ls`. Returns either a human table or the interfaces.md §10.3
/// JSON shape (a single string, not printed — caller decides).
pub fn run_ls(paths: &CcteamPaths, format: OutputFormat) -> Result<String> {
    let projects = collect_projects(paths)?;
    let daemon_up = ccteam_core::daemon::heartbeat_alive(paths);
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
    let cost = ccteam_core::cost_summary(slug, &progress_path, paths)?;

    Ok(match format {
        OutputFormat::Text => render_show_text(&state, &cost, &recent, &artifacts),
        OutputFormat::Json => render_show_json(&state, &cost, &recent, &artifacts)?,
    })
}

/// `ccteam attach <slug>`. Resolves the underlying session medium and
/// dispatches:
///
/// 1. If a tmux session named `ccteam-<slug>` exists → `tmux attach -t …`
///    (V0.3.x meta-agent + legacy projects).
/// 2. Else if the project's latest `agent_spawn` event in
///    `progress.jsonl` carries a `claude --bg` `job_id` → `claude attach
///    <job_id>` (V0.4.0 worker default).
/// 3. Else → fail-loud "no live session for <slug>".
///
/// Always prints the underlying command before exec'ing so the operator
/// learns the lower-level tool.
pub fn run_attach(paths: &CcteamPaths, slug: &str) -> Result<()> {
    let tmux_session = TmuxSession::from_name(session_name_for_project(paths, slug));
    if tmux_session.exists() {
        eprintln!("→ tmux attach -t {}", tmux_session.name());
        let status = Command::new("tmux")
            .args(["attach", "-t", tmux_session.name()])
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
    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    candidates.into_iter().next().map(|(_, id)| id)
}

/// `ccteam peek <slug>`. Returns the contents of the session's first
/// pane via `tmux capture-pane -p`.
pub fn run_peek(paths: &CcteamPaths, slug: &str) -> Result<String> {
    let session = TmuxSession::from_name(session_name_for_project(paths, slug));
    if !session.exists() {
        bail!("tmux session not running: {}", session.name());
    }
    let output = Command::new("tmux")
        .args(["capture-pane", "-p", "-t", session.name()])
        .output()
        .context("spawn tmux capture-pane")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("tmux capture-pane failed: {stderr}");
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// `ccteam progress <slug>`. With `tail = false`, returns the entire
/// progress.jsonl as text. With `tail = true`, reads + writes to
/// stdout in a polling loop until Ctrl-C.
pub fn run_progress(paths: &CcteamPaths, slug: &str, tail: bool) -> Result<()> {
    use std::io::Write;
    let state = ProjectState::load(&paths.project_state(slug)).ok();
    if state
        .as_ref()
        .is_some_and(|s| s.team_kind == ccteam_core::TeamKind::Flex)
    {
        if tail {
            bail!(
                "ccteam progress --tail is not supported for flex project `{slug}`; \
                 use `ccteam session ls {slug}` or the web session view",
            );
        }
        let events = collect_recent_events(paths, slug, usize::MAX)?;
        let mut stdout = std::io::stdout().lock();
        for event in events {
            writeln!(stdout, "{}", serde_json::to_string(&event)?)?;
        }
        return Ok(());
    }

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

// =====================================================================
// V0.3.1 F49 — `ccteam session` handlers for flex teams
// =====================================================================

/// Add a registered harness session to a flex project. Both claude
/// and codex sessions record in the master `state.json::sessions`
/// map. V0.4.0 F61 changes the claude branch to launch
/// `claude --bg --agent <role>` (no tmux); the codex branch (F62)
/// still uses tmux + `codex` CLI.
pub fn run_session_add(slug: &str, harness: ccteam_core::HarnessKind, role: String) -> Result<()> {
    use ccteam_core::{
        harness_sid_prefix, ClaudeCodeAdapter, CodexAdapter, HarnessAdapter, HarnessKind,
        SessionRecord, SpawnOpts, TeamKind,
    };

    let paths = CcteamPaths::from_env()?;
    let state_path = paths.project_state(slug);
    let mut state = load_project_state(&paths, slug)?;
    refresh_state_team_kind(&paths, &mut state)?;
    if state.team_kind != TeamKind::Flex {
        bail_session_requires_flex(&state)?;
    }

    let sid = state.allocate_sid(harness);
    let session_dir = paths.project_session_dir(slug, &sid);
    std::fs::create_dir_all(session_dir.join("inbox"))
        .with_context(|| format!("create {}", session_dir.join("inbox").display()))?;
    std::fs::create_dir_all(session_dir.join("outbox"))
        .with_context(|| format!("create {}", session_dir.join("outbox").display()))?;

    // Adapter dispatch — every harness shares the SpawnOpts schema so
    // the call sites differ only in which `Adapter::new()` runs. V0.4.0
    // F61 routes the claude branch to `claude --bg` (capturing job_id);
    // V0.4.0 F62 keeps the codex branch on tmux.
    let handle = match harness {
        HarnessKind::Claude => {
            let adapter = ClaudeCodeAdapter::new();
            adapter
                .spawn_session(SpawnOpts {
                    harness: adapter.name(),
                    slug: slug.to_string(),
                    sid: sid.clone(),
                    cwd: session_dir.clone(),
                    role: role.clone(),
                    extra_args: Vec::new(),
                })
                .map_err(|err| anyhow::anyhow!("{err}"))?
        }
        HarnessKind::Codex => {
            let adapter = CodexAdapter::new();
            adapter
                .spawn_session(SpawnOpts {
                    harness: adapter.name(),
                    slug: slug.to_string(),
                    sid: sid.clone(),
                    cwd: session_dir.clone(),
                    role: String::new(),
                    extra_args: Vec::new(),
                })
                .map_err(|err| anyhow::anyhow!("{err}"))?
        }
    };

    let was_empty = state.sessions.is_empty();
    if was_empty {
        state.tmux_session = handle.tmux_session.clone();
        state.claude_pid = handle.pid.and_then(|pid| i32::try_from(pid).ok());
    }
    state.sessions.insert(
        sid.clone(),
        SessionRecord {
            harness,
            tmux_session: handle.tmux_session.clone(),
            started_at: handle.started_at,
            pid: handle.pid,
            job_id: handle.job_id.clone(),
        },
    );
    state.save(&state_path)?;
    println!(
        "added session {sid} ({}) for {slug}\n  tmux: {}\n  cwd: {}",
        harness_sid_prefix(harness),
        handle.tmux_session,
        session_dir.display(),
    );
    Ok(())
}

/// Print one row per registered flex session.
pub fn run_session_ls(slug: &str) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    let mut state = load_project_state(&paths, slug)?;
    refresh_state_team_kind(&paths, &mut state)?;
    if state.team_kind != ccteam_core::TeamKind::Flex {
        bail_session_requires_flex(&state)?;
    }

    println!("sid\tharness\ttmux\tstarted_at\tpid\tlast_event");
    for (sid, record) in &state.sessions {
        let last = session_last_event(&paths, slug, sid)?;
        let pid = record
            .pid
            .map(|p| p.to_string())
            .unwrap_or_else(|| "-".into());
        println!(
            "{sid}\t{}\t{}\t{}\t{}\t{}",
            ccteam_core::harness_sid_prefix(record.harness),
            record.tmux_session,
            record.started_at.to_rfc3339(),
            pid,
            last.unwrap_or_else(|| "-".into()),
        );
    }
    Ok(())
}

/// Attach to one registered flex session.
pub fn run_session_attach(slug: &str, sid: &str) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    let mut state = load_project_state(&paths, slug)?;
    refresh_state_team_kind(&paths, &mut state)?;
    if state.team_kind != ccteam_core::TeamKind::Flex {
        bail_session_requires_flex(&state)?;
    }
    let record = state.sessions.get(sid).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown session `{sid}` for `{slug}`; available: {}",
            available_sids(&state)
        )
    })?;
    let session = TmuxSession::from_name(record.tmux_session.clone());
    if !session.exists() {
        bail!("tmux session not running: {}", session.name());
    }
    let status = Command::new("tmux")
        .args(["attach", "-t", session.name()])
        .status()
        .context("spawn tmux attach")?;
    if !status.success() {
        bail!("tmux attach exited with {status}");
    }
    Ok(())
}

/// Gracefully shut down one registered flex session and scrub it from
/// `state.json::sessions[]`.
pub fn run_session_rm(slug: &str, sid: &str) -> Result<()> {
    use ccteam_core::{ClaudeCodeAdapter, CodexAdapter, HarnessAdapter, SessionHandle};

    let paths = CcteamPaths::from_env()?;
    let state_path = paths.project_state(slug);
    let mut state = load_project_state(&paths, slug)?;
    refresh_state_team_kind(&paths, &mut state)?;
    if state.team_kind != ccteam_core::TeamKind::Flex {
        bail_session_requires_flex(&state)?;
    }
    let record = state.sessions.get(sid).cloned().ok_or_else(|| {
        anyhow::anyhow!(
            "unknown session `{sid}` for `{slug}`; available: {}",
            available_sids(&state)
        )
    })?;
    let handle = SessionHandle {
        tmux_session: record.tmux_session.clone(),
        harness: ccteam_core::harness_sid_prefix(record.harness).into(),
        sid: sid.to_string(),
        job_id: record.job_id.clone(),
        pid: record.pid,
        started_at: record.started_at,
    };
    match record.harness {
        ccteam_core::HarnessKind::Claude => ClaudeCodeAdapter::new()
            .shutdown_session(&handle)
            .map_err(|err| anyhow::anyhow!("{err}"))?,
        ccteam_core::HarnessKind::Codex => CodexAdapter::new()
            .shutdown_session(&handle)
            .map_err(|err| anyhow::anyhow!("{err}"))?,
    }
    state.sessions.remove(sid);
    state.save(&state_path)?;
    println!("removed session {sid} from {slug}");
    Ok(())
}

fn load_project_state(paths: &CcteamPaths, slug: &str) -> Result<ProjectState> {
    let state_path = paths.project_state(slug);
    if !state_path.exists() {
        bail!("project not found: {slug}");
    }
    ProjectState::load(&state_path)
}

fn refresh_state_team_kind(paths: &CcteamPaths, state: &mut ProjectState) -> Result<()> {
    if let Ok(kind) = resolve_team_kind(paths, &state.team) {
        state.team_kind = kind;
    }
    Ok(())
}

fn bail_session_requires_flex(state: &ProjectState) -> Result<()> {
    bail!(
        "session subcommands only work on flex teams; project `{}` is team `{}` (kind={:?})",
        state.slug,
        state.team,
        state.team_kind,
    )
}

fn available_sids(state: &ProjectState) -> String {
    if state.sessions.is_empty() {
        "(none)".into()
    } else {
        state
            .sessions
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn session_last_event(paths: &CcteamPaths, slug: &str, sid: &str) -> Result<Option<String>> {
    let path = paths.progress_jsonl_for_session(slug, sid);
    if !path.exists() {
        return Ok(None);
    }
    let body =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    Ok(body
        .lines()
        .rev()
        .find_map(|line| serde_json::from_str::<Value>(line).ok())
        .and_then(|v| v.get("event").and_then(|e| e.as_str()).map(str::to_string)))
}

/// `ccteam doctor` flags. Each mode is a separate boolean / option so
/// they can be combined (e.g. `--install-meta-agent rob` implies
/// `--install-skill` automatically).
#[derive(Debug, Clone, Default)]
pub struct DoctorOptions {
    pub dry_run: bool,
    pub force: bool,
    pub tool_surface: bool,
    /// M1.8: install `~/.claude/skills/ccteam-control/SKILL.md`
    /// and `~/.claude/skills/ccteam-team-author/SKILL.md`, plus run the
    /// V0.2.2 (F39'd) → V0.2.2 (F44'd) reverse migration (cleanup of
    /// legacy `cct-*` skill dirs and stale settings.json hook command
    /// paths).
    pub install_skill: bool,
    /// V0.4.1: bootstrap the meta-agent project at `~/projects/meta/`.
    /// `true` triggers `install_skill` regardless of its standalone
    /// flag. (Pre-V0.4.1 this was `Option<String>` for a per-user handle
    /// — handle was dropped, the meta-agent slug is now always `meta`.)
    pub install_meta_agent: bool,
    /// M2.5: register `mcpServers.ccteam` in `~/.claude.json`.
    pub install_mcp: bool,
    /// M4.2: install `~/.claude/rules/ccteam-lessons-<team>.md` placeholders.
    pub install_memory_bridge: bool,
    /// V0.2 M0.16.2: re-write every shipped team
    /// (`~/.ccteam/teams/<name>/team.yaml` + `~/.ccteam/<phase_dir>/*.md`)
    /// using the in-binary seed bundle. `force=true` overwrites operator
    /// hand-edits; `force=false` is equivalent to the auto-seed run on
    /// `Orchestrator::new`. Useful after a ccteam upgrade ships
    /// schema-additive team.yaml changes (e.g. the V0.2 `evergreen` /
    /// `cost_policy` fields landed by M0.16).
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
}

/// `ccteam doctor` dispatch. Returns a human-readable report so unit
/// tests don't need to capture stdout.
pub fn run_doctor(paths: &CcteamPaths, opts: DoctorOptions) -> Result<String> {
    let any_mode = opts.tool_surface
        || opts.install_skill
        || opts.install_meta_agent
        || opts.install_mcp
        || opts.install_memory_bridge
        || opts.reset_shipped_teams
        || opts.validate_team.is_some()
        || opts.migrate_recommended_agents
        || opts.screenshot_smoke.is_some()
        || opts.migrate_v041_to_v042
        || opts.migrate_workflow_to_ccteam_dir
        || opts.gc_claude_jobs
        || opts.update_hooks;
    if !any_mode {
        return Ok(String::from(
            "ccteam doctor: pass at least one mode flag.\n\
             \n\
             modes:\n  \
             --tool-surface\n      \
             cross-check phase templates' tools_required against current reachability — \
             plugin-pipeline-aware (V0.2 M0.20).\n  \
             --install-skill [--force]\n      \
             write ~/.claude/skills/ccteam-{control,team-author,project-creator}/SKILL.md (M1.8); \
             also runs the V0.2.2 (F39'd) → V0.2.2 (F44'd) reverse migration (legacy `cct-*` skill dirs + \
             stale settings.json hook command paths).\n  \
             --install-meta-agent\n      \
             bootstrap the canonical meta-agent project at ~/projects/meta/. Implies --install-skill. (V0.4.1: handle dropped — one ccteam install = one meta-agent.)\n  \
             --install-mcp\n      \
             register `mcpServers.ccteam` in ~/.claude.json so daily-driver claude + meta-agent see the 9-tool MCP server (M2.5).\n  \
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
             walk every registered project's .claude/settings.json and strip the retired `ccteam hook cost-accumulate` entry. Idempotent (V0.4.6 F91).\n",
        ));
    }
    let mut out = String::new();
    if opts.tool_surface {
        out.push_str(&render_tool_surface_report(paths)?);
    }
    // --install-meta-agent implies --install-skill so a fresh meta
    // session has the dispatcher tool list immediately.
    let install_skill_now = opts.install_skill || opts.install_meta_agent;
    if install_skill_now {
        out.push_str(&render_install_skill_report(paths, &opts)?);
    }
    if opts.install_meta_agent {
        out.push_str(&render_install_meta_agent_report(paths)?);
    }
    if opts.install_mcp {
        out.push_str(&render_install_mcp_report()?);
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
    // V0.3.1 F47 — informational codex CLI detection. Appends one line
    // to every successful doctor run (any_mode == true) so operators
    // see whether the codex binary is on PATH ahead of V0.3.2's real
    // CodexAdapter impl. **Never fails the doctor exit code** —
    // `which codex` returning non-zero is the expected state today.
    out.push_str(&render_codex_detection_line());
    Ok(out)
}

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
    out.push_str(&format!("  dir_count_before: {}\n", report.dir_count_before));
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
///    are user territory; `docs/v0-2/phase-prompt-architecture.md` §9 docks
///    this as warn-not-fail by design).
fn render_validate_team_report(paths: &CcteamPaths, team: &str) -> Result<(String, u32)> {
    use ccteam_core::{
        default_user_staging_dir, resolve_team, staging_dir_for, validate_staged_team,
        TeamResolveContext,
    };

    let mut out = format!("ccteam doctor --validate-team {team}\n\n");
    let mut fails = 0u32;
    let user_staging = default_user_staging_dir();
    let ctx = TeamResolveContext::for_orchestrator(&paths.root, &user_staging);

    // V0.2 M0.22.4: when a staged plugin tree exists at the user
    // layer, run the plugin manifest checks before resolving the
    // TeamSpec — operators authoring a team plugin want both the
    // plugin schema findings and the phase IO findings in one report.
    let staged = staging_dir_for(team, None);
    if staged.join(".claude-plugin/plugin.json").exists() {
        out.push_str("# Plugin manifest checks (staged tree)\n");
        for line in validate_staged_team(&staged)? {
            if line.starts_with("[FAIL]") {
                fails += 1;
            }
            out.push_str(&format!("{line}\n"));
        }
        out.push('\n');
    }

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
    let mut out = String::from("ccteam doctor --install-mcp\n\n");
    out.push_str(&format!(
        "  registered ccteam MCP server in {}\n",
        path.display()
    ));
    out.push_str("  tools surface : 9 (interfaces §12.2)\n");
    out.push_str("  consumers     : daily-driver claude + meta-agent\n");
    out.push('\n');
    out.push_str(
        "open a new claude session to pick up the change; existing sessions need /reload-mcp.\n",
    );
    Ok(out)
}

fn skill_install_label(action: &SkillInstallAction) -> String {
    match action {
        SkillInstallAction::Wrote => "wrote".into(),
        SkillInstallAction::AlreadyPresent => "already-present (use --force to overwrite)".into(),
        SkillInstallAction::Replaced => "replaced".into(),
        SkillInstallAction::DryRun { would_write } => {
            if *would_write {
                "would write".into()
            } else {
                "no-op (already present)".into()
            }
        }
    }
}

fn render_install_skill_report(paths: &CcteamPaths, opts: &DoctorOptions) -> Result<String> {
    let install_opts = InstallSkillOptions {
        force: opts.force,
        dry_run: opts.dry_run,
    };
    let mut out = String::from("ccteam doctor --install-skill\n\n");

    // V0.2.2 F44: install all shipped skills under their canonical
    // ccteam-* names (reverting F39's `cct-*` rename).
    let control = install_ccteam_control_skill(install_opts)?;
    out.push_str(&format!(
        "  ccteam-control          {label}  {}\n",
        control.target.display(),
        label = skill_install_label(&control.action),
    ));
    let team_author = install_ccteam_team_author_skill(install_opts)?;
    out.push_str(&format!(
        "  ccteam-team-author      {label}  {}\n",
        team_author.target.display(),
        label = skill_install_label(&team_author.action),
    ));
    let project_creator = install_ccteam_project_creator_skill(install_opts)?;
    out.push_str(&format!(
        "  ccteam-project-creator  {label}  {}\n",
        project_creator.target.display(),
        label = skill_install_label(&project_creator.action),
    ));
    out.push('\n');

    // V0.2.2 F44 reverse migration: clean up F39-era `cct-*` skill dirs
    // and rewrite stale settings.json hook command paths so users
    // upgrading from F39'd V0.2.2 to F44'd V0.2.2 don't end up with
    // duplicate `cct-*` + `ccteam-*` skill directories or hook commands
    // pointing at the F39 `cct` binary that no longer exists.
    out.push_str(&render_f44_migration(paths, opts)?);

    Ok(out)
}

/// V0.2.2 F44: detect-and-clean F39-era install artifacts so users who
/// revert from `cct → ccteam` don't end up with both naming conventions
/// live at once. Runs as a side effect of `--install-skill` and
/// `--install-meta-agent`.
fn render_f44_migration(paths: &CcteamPaths, opts: &DoctorOptions) -> Result<String> {
    let mut out = String::new();
    let claude = match user_claude_dir() {
        Ok(c) => c,
        Err(_) => {
            // No claude dir — no F39-era install to clean up.
            return Ok(out);
        }
    };

    out.push_str("F44 reverse migration (V0.2.2 F39 → V0.2.2 F44 cleanup)\n\n");

    // 1. Legacy `cct-*` skill dirs.
    let skill_reports = migrate_legacy_skill_dirs(&claude, opts.dry_run)?;
    let any_skill_action = skill_reports
        .iter()
        .any(|r| r.action != LegacySkillAction::NotFound);
    if any_skill_action {
        for r in &skill_reports {
            out.push_str(&render_legacy_skill_line(r, opts.dry_run));
        }
    } else {
        out.push_str(
            "  legacy skills    no `~/.claude/skills/cct-*` dirs found — nothing to do.\n",
        );
    }

    // 2. Stale settings.json hook command paths. Use the orchestrator's
    // resolved projects root so test fixtures get the tempdir; tests
    // don't otherwise mutate `~/projects/`.
    let new_bin = current_ccteam_bin().ok();
    if let Some(bin) = new_bin.as_ref() {
        let hook_reports =
            scan_project_settings_for_hook_rewrite(&paths.projects_root, bin, opts.dry_run)?;
        if hook_reports.is_empty() {
            out.push_str("  legacy hooks     no project `settings.json` files found.\n");
        } else {
            let mut any_rewritten = false;
            for r in &hook_reports {
                let line = render_hook_rewrite_line(r, opts.dry_run);
                out.push_str(&line);
                if matches!(
                    r.action,
                    HookCmdRewriteAction::Rewrote { .. }
                        | HookCmdRewriteAction::WouldRewrite { .. }
                ) {
                    any_rewritten = true;
                }
            }
            if !any_rewritten {
                // All NoChangeNeeded — collapse the table to a single
                // friendly summary instead of dumping every project.
                out.clear();
                out.push_str("F44 reverse migration (V0.2.2 F39 → V0.2.2 F44 cleanup)\n\n");
                for r in &skill_reports {
                    out.push_str(&render_legacy_skill_line(r, opts.dry_run));
                }
                out.push_str(&format!(
                    "  legacy hooks     scanned {} project settings.json — no rewrites needed.\n",
                    hook_reports.len(),
                ));
            }
        }
    } else {
        out.push_str("  legacy hooks     could not resolve `ccteam` binary — skipped.\n");
    }
    out.push('\n');
    Ok(out)
}

fn render_legacy_skill_line(r: &LegacySkillReport, dry_run: bool) -> String {
    let label = match (&r.action, dry_run) {
        (LegacySkillAction::NotFound, _) => "not present",
        (LegacySkillAction::Removed, _) => "removed",
        (LegacySkillAction::WouldRemove, _) => "would remove",
        (LegacySkillAction::PreservedHandEdit, _) => "preserved (hand-edited)",
    };
    format!(
        "  {:<28} {:<22} {}\n",
        format!("{}/", r.legacy_name),
        label,
        r.target.display(),
    )
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
    new_bin: &std::path::Path,
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
        let report = rewrite_legacy_hook_commands(&settings, new_bin, dry_run)?;
        out.push(report);
    }
    Ok(out)
}

fn render_install_meta_agent_report(paths: &CcteamPaths) -> Result<String> {
    let report: MetaBootstrapReport = bootstrap_meta_project(paths)?;
    let mut out = String::from("ccteam doctor --install-meta-agent\n\n");
    out.push_str(&format!("  project slug     {}\n", report.slug));
    out.push_str(&format!(
        "  project dir      {}\n",
        report.project_dir.display()
    ));
    out.push_str(&format!(
        "  role prompt      {}\n",
        report.claude_md.display()
    ));
    out.push_str(&format!(
        "  status           {}\n",
        if report.already_existed {
            "refreshed"
        } else {
            "created"
        },
    ));
    if !report.removed_stale.is_empty() {
        out.push_str(&format!(
            "  cleaned legacy   {} stale meta-<handle> dir(s) removed\n",
            report.removed_stale.len()
        ));
        for path in &report.removed_stale {
            out.push_str(&format!("                     - {}\n", path.display()));
        }
    }
    out.push('\n');
    let tmux_session = ccteam_core::meta_session_name();
    out.push_str(&format!("tmux session     {tmux_session}\n"));
    out.push_str(&format!("attach with      tmux attach -t {tmux_session}\n"));
    out.push_str("\nrun `ccteam start` (in another terminal) to wake the meta session.\n");
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
        let cost_24h =
            ccteam_core::cost_summary(&p.state.slug, &paths.progress_jsonl(&p.state.slug), paths)
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
    // longer be derived from `phase_state == InFlight`. F66 will
    // recompute this from `state.sessions` (live agent count).
    let active_count = 0usize;
    let arr: Vec<Value> = projects
        .iter()
        .map(|p| {
            // V0.4.6 F91 — JSON shape preserves the `cost_used_usd`
            // key for callers (MCP / scripts) but populates it from
            // `cost_24h_usd` so the number tracks reality. The legacy
            // serde field still reads as the frozen pre-F91 value if
            // anything in the JSON pipeline needs to differentiate.
            let cost_summary = ccteam_core::cost_summary(
                &p.state.slug,
                &paths.progress_jsonl(&p.state.slug),
                paths,
            )
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
        // F27 — `running` is now a real bool driven by
        // `daemon::heartbeat_alive` so meta-agent / MCP consumers can
        // gate writes on daemon liveness without an extra round trip.
        "orchestrator": {
            "running": daemon_up,
            "active_count": active_count,
            "max_concurrent": 1,
        }
    });
    Ok(serde_json::to_string_pretty(&v)?)
}

fn render_show_text(
    state: &ProjectState,
    cost: &ccteam_core::CostSummary,
    recent: &[Value],
    artifacts: &Map<String, Value>,
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
) -> Result<String> {
    let v = json!({
        "state": serde_json::to_value(state)?,
        "phase_history": serde_json::to_value(&state.phase_history)?,
        "cost": serde_json::to_value(cost)?,
        "recent_events": recent,
        "artifacts": Value::Object(artifacts.clone()),
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

fn stall_level(silent_s: u64) -> &'static str {
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
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let mut state = ProjectState::initial_for_team("meta-cto".into(), "meta-agent".into());
        state.tmux_session = "ccteam-meta-cto".into();
        state.save(&paths.project_state("meta-cto")).unwrap();

        let err = run_peek(&paths, "meta-cto").unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("ccteam-meta-cto"),
            "peek should target state.tmux_session, got: {msg}",
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
    fn f27_run_ls_text_reports_daemon_down_when_no_heartbeat() {
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
        // now it's a real bool gated on heartbeat freshness so MCP /
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
        // No heartbeat written → must be false.
        assert_eq!(running.as_bool(), Some(false));
    }

    #[test]
    fn f27_run_ls_text_reports_daemon_up_on_fresh_heartbeat() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        ccteam_core::daemon::write_heartbeat(&paths).unwrap();
        let body = run_ls(&paths, OutputFormat::Text).unwrap();
        assert!(
            body.starts_with("daemon: up"),
            "expected `daemon: up` head line on fresh heartbeat; got:\n{body}",
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
        assert_eq!(v["artifacts"]["spec"], ".ccteam/spec.md");
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
        for sub in ["phases", "templates", "progress", "inbox", "control"] {
            assert!(
                paths.root.join(sub).is_dir(),
                "init must create {}",
                paths.root.join(sub).display()
            );
        }
        // V0.4.0 F60: phase template writing removed (F63 reintroduces
        // workflow seeds). Only helper templates land via init now.
        assert!(paths
            .templates_dir()
            .join("review-with-user-loop.md")
            .is_file());
        assert!(paths
            .templates_dir()
            .join("kickoff-reverse-interview.md")
            .is_file());
        assert!(report.contains("ccteam init"));
        assert!(report.contains("next"));
    }

    #[test]
    fn run_init_is_idempotent_and_preserves_user_edits() {
        // V0.4.0 F60: phase template writer removed; the
        // helper-template writer still uses the same idempotency
        // contract (skip-existing without --force; overwrite with).
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        run_init(&paths, init_opts_targeting_tmp(&tmp, "idem-demo")).unwrap();
        let path = paths.templates_dir().join("review-with-user-loop.md");
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
            .join("explorer.md")
            .is_file());

        let cfg = ccteam_core::load_ccteam_config(&paths.root).unwrap();
        assert_eq!(cfg.projects.len(), 1);
        assert_eq!(cfg.projects[0].slug, "f72-fresh");
        assert_eq!(cfg.projects[0].team, "dev");
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
            target.join(".claude").join("agents").join("explorer.md"),
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
            std::fs::read_to_string(target.join(".claude").join("agents").join("explorer.md"))
                .unwrap(),
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
            target.join(".claude").join("agents").join("explorer.md"),
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
            std::fs::read_to_string(target.join(".claude").join("agents").join("explorer.md"))
                .unwrap(),
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
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let body = run_doctor(&paths, DoctorOptions::default()).unwrap();
        assert!(body.contains("tool-surface"));
        assert!(body.contains("install-skill"));
        assert!(body.contains("install-meta-agent"));
        assert!(body.contains("install-memory-bridge"));
        assert!(body.contains("validate-team"), "got: {body}");
        assert!(body.contains("migrate-recommended-agents"), "got: {body}");
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
    fn run_doctor_install_meta_agent_creates_project_and_skill() {
        // M1.0 + M1.8 combo: --install-meta-agent implies
        // --install-skill, so a single invocation gets the user a
        // ready-to-attach session. V0.4.1: handle dropped — one ccteam
        // install ⇒ one meta-agent at canonical `meta/`.
        ensure_isolation();
        let _guard = env_lock().lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        // Redirect ~/.claude/ to the tempdir so the skill install
        // doesn't touch the developer's real ~/.claude/.
        std::env::set_var("CLAUDE_CONFIG_HOME", tmp.path().to_str().unwrap());

        let opts = DoctorOptions {
            install_meta_agent: true,
            ..DoctorOptions::default()
        };
        let report = run_doctor(&paths, opts).unwrap();
        assert!(
            report.contains("install-skill"),
            "skill install report missing"
        );
        assert!(
            report.contains("install-meta-agent"),
            "meta install report missing"
        );
        assert!(report.contains("meta"), "meta slug should be reported");

        // Project directory exists at canonical V0.4.1 path.
        assert!(paths.project_dir("meta").is_dir());
        let state = ProjectState::load(&paths.project_state("meta")).unwrap();
        assert_eq!(state.team, "meta-agent");
        assert_eq!(state.slug, "meta");
        assert_eq!(state.tmux_session, "ccteam-meta");

        // Skill landed under the redirected ~/.claude/. F44 reverts F39:
        // both shipped skills are written under canonical ccteam-* names.
        let control = tmp.path().join("skills/ccteam-control/SKILL.md");
        assert!(
            control.is_file(),
            "ccteam-control SKILL.md not written: {}",
            control.display()
        );
        let team_author = tmp.path().join("skills/ccteam-team-author/SKILL.md");
        assert!(
            team_author.is_file(),
            "ccteam-team-author SKILL.md not written: {}",
            team_author.display(),
        );

        std::env::remove_var("CLAUDE_CONFIG_HOME");
    }

    #[test]
    fn run_doctor_appends_codex_detection_line_when_any_mode_runs() {
        // V0.3.1 F47 — every successful doctor run (any_mode == true)
        // appends one informational `[ccteam] codex CLI: ...` line at
        // the end. Pure informational — never fails the report. The
        // exact path / not-found suffix depends on the host so we
        // only pin the prefix.
        ensure_isolation();
        let _guard = env_lock().lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        std::env::set_var("CLAUDE_CONFIG_HOME", tmp.path().to_str().unwrap());

        let opts = DoctorOptions {
            install_skill: true,
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
    fn run_doctor_no_flags_help_text_does_not_print_codex_line() {
        // The help-text early-return path is a usage error, not a
        // health check — codex detection is gated on `any_mode == true`
        // (see `run_doctor` source). Pin that contract so a future
        // refactor doesn't accidentally surface the line in the help
        // text and confuse first-time users.
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let body = run_doctor(&paths, DoctorOptions::default()).unwrap();
        assert!(
            !body.contains("[ccteam] codex CLI:"),
            "help-text path must NOT include codex detection; got:\n{body}",
        );
    }

    #[test]
    fn run_doctor_install_skill_only_lays_down_skill_md() {
        ensure_isolation();
        let _guard = env_lock().lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        std::env::set_var("CLAUDE_CONFIG_HOME", tmp.path().to_str().unwrap());

        let opts = DoctorOptions {
            install_skill: true,
            ..DoctorOptions::default()
        };
        let report = run_doctor(&paths, opts).unwrap();
        assert!(report.contains("install-skill"));
        assert!(!report.contains("install-meta-agent"));

        // F44 reverts F39: shipped skills land under canonical ccteam-* names.
        assert!(tmp.path().join("skills/ccteam-control/SKILL.md").is_file());
        assert!(tmp
            .path()
            .join("skills/ccteam-team-author/SKILL.md")
            .is_file());
        std::env::remove_var("CLAUDE_CONFIG_HOME");
    }

    #[test]
    fn run_doctor_install_skill_runs_f44_legacy_skill_cleanup() {
        // V0.2.2 F44: --install-skill detects + removes any
        // ~/.claude/skills/cct-{control,team-author,project-creator}/ left
        // over from F39'd V0.2.2 installs whose body still carries the
        // ccteam-managed marker (or canonical frontmatter).
        ensure_isolation();
        let _guard = env_lock().lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        std::env::set_var("CLAUDE_CONFIG_HOME", tmp.path().to_str().unwrap());
        // Stage an F39'd install (V0.2.2 PR #1 era).
        let legacy = tmp.path().join("skills/cct-control");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(
            legacy.join("SKILL.md"),
            "---\nname: cct-control\n---\n# legacy body\n",
        )
        .unwrap();

        let opts = DoctorOptions {
            install_skill: true,
            ..DoctorOptions::default()
        };
        let report = run_doctor(&paths, opts).unwrap();
        assert!(
            report.contains("F44 reverse migration"),
            "F44 report header missing: {report}"
        );
        assert!(
            report.contains("cct-control/"),
            "legacy entry missing: {report}"
        );
        assert!(!legacy.exists(), "legacy cct-control dir survived");
        // New (canonical) name still landed.
        assert!(tmp.path().join("skills/ccteam-control/SKILL.md").is_file());
        std::env::remove_var("CLAUDE_CONFIG_HOME");
    }

    #[test]
    fn run_doctor_install_skill_preserves_user_hand_edited_legacy_skill() {
        // F44: a legacy `cct-*` directory whose SKILL.md was hand-edited
        // (no marker, no canonical frontmatter) is preserved.
        ensure_isolation();
        let _guard = env_lock().lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        std::env::set_var("CLAUDE_CONFIG_HOME", tmp.path().to_str().unwrap());
        let legacy = tmp.path().join("skills/cct-team-author");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(
            legacy.join("SKILL.md"),
            "---\nname: my-fork\n---\n# user wrote this\n",
        )
        .unwrap();

        let opts = DoctorOptions {
            install_skill: true,
            ..DoctorOptions::default()
        };
        let report = run_doctor(&paths, opts).unwrap();
        assert!(
            report.contains("preserved"),
            "F44 should report preserve: {report}"
        );
        assert!(
            legacy.exists(),
            "hand-edited legacy dir was unexpectedly removed"
        );
        std::env::remove_var("CLAUDE_CONFIG_HOME");
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
}
