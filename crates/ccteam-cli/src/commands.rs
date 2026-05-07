//! Command handlers for `ccteam {new, ls, show, attach, peek, progress,
//! resume}`. Pure where possible (`run_ls` / `run_show` return the
//! formatted string instead of printing) so unit tests don't need a
//! real terminal or running orchestrator.

use std::process::Command;

use anyhow::{bail, Context, Result};
use chrono::Utc;
use serde_json::{json, Map, Value};

use ccteam_core::{
    bootstrap_meta_project, bootstrap_project, current_ccteam_bin, install_ccteam_control_skill,
    link_recommended_agents, pick_unused_slug, user_claude_dir,
    write_all_global_team_templates, write_global_helper_templates,
    write_global_phase_templates, AgentLinkAction, AgentLinkReport, CcteamPaths,
    InstallSkillOptions, LinkOptions, MetaBootstrapReport, OutboxEventKind, OutboxMessage,
    PhaseHistoryEntry, PhaseState, PhaseTemplate, ProjectState, SessionMailbox,
    SkillInstallAction, TeamSpec,
    ToolSurfaceSnapshot, BUILTIN_SUBAGENTS, HELPER_TEMPLATES, PHASE_TEMPLATES,
};
use ccteam_core::tmux::TmuxSession;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    Text,
    Json,
}

/// Options passed from the `ccteam init` argument parser.
#[derive(Debug, Clone, Copy, Default)]
pub struct InitOptions {
    /// Overwrite existing global phase templates instead of skipping
    /// them (default: false, so hand edits stick across re-init).
    pub force: bool,
}

/// `ccteam init`. Creates `~/.ccteam/{phases,progress,inbox,control,
/// queue,memory,state}`, unpacks the embedded phase templates into
/// `~/.ccteam/phases/`, and runs a quick health check. Returns a
/// human-readable report.
pub fn run_init(paths: &CcteamPaths, opts: InitOptions) -> Result<String> {
    use std::process::Command;

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
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("create {}", dir.display()))?;
    }

    write_global_phase_templates(&paths.root, opts.force)
        .with_context(|| format!("unpack phase templates to {}", paths.phases_dir().display()))?;
    write_global_helper_templates(&paths.root, opts.force).with_context(|| {
        format!(
            "unpack helper templates to {}",
            paths.templates_dir().display()
        )
    })?;
    // M3.3: also unpack every embedded team.yaml + non-dev team's
    // phase set so multi-team installs come up out of the box.
    // dev's phase set is duplicated here (idempotent — `force=false`
    // skips files already on disk) so the legacy `phases/` dir stays
    // populated even when this is the only template writer ever called.
    write_all_global_team_templates(&paths.root, opts.force).with_context(|| {
        format!(
            "unpack team bundles (team.yaml + per-team phases) to {}",
            paths.root.display(),
        )
    })?;

    let bin = current_ccteam_bin().ok();

    let claude = Command::new("claude").arg("--version").output();
    let tmux = Command::new("tmux").arg("-V").output();

    let mut out = String::new();
    out.push_str(&format!("✓ created {}\n", paths.root.display()));
    out.push_str(&format!(
        "✓ unpacked {} phase templates → {}\n",
        PHASE_TEMPLATES.len(),
        paths.phases_dir().display()
    ));
    out.push_str(&format!(
        "✓ unpacked {} helper templates → {}\n",
        HELPER_TEMPLATES.len(),
        paths.templates_dir().display()
    ));
    out.push_str("\nhealth check:\n");
    match &claude {
        Ok(o) if o.status.success() => out.push_str(&format!(
            "  claude   : {}\n",
            String::from_utf8_lossy(&o.stdout).trim()
        )),
        _ => out.push_str("  claude   : NOT FOUND on PATH (install: https://claude.com/claude-code)\n"),
    }
    match &tmux {
        Ok(o) if o.status.success() => out.push_str(&format!(
            "  tmux     : {}\n",
            String::from_utf8_lossy(&o.stdout).trim()
        )),
        _ => out.push_str("  tmux     : NOT FOUND on PATH (apt install tmux / brew install tmux)\n"),
    }
    match &bin {
        Some(p) => out.push_str(&format!("  ccteam   : {}\n", p.display())),
        None => out.push_str("  ccteam   : current_exe() failed (binary path unresolved)\n"),
    }

    out.push_str("\nnext:\n");
    out.push_str("  ccteam new \"<your one-line request>\"\n");
    out.push_str("  ccteam start --foreground   # in another terminal\n");
    Ok(out)
}

/// `ccteam new "request" --team <name>`. Bootstraps a project on disk
/// and returns the chosen slug. Side effects: creates
/// `~/projects/<slug>/...`. `team` is recorded in state.json so the
/// orchestrator can route this project through the matching phase set.
///
/// **M3.3 fail-fast**: non-dev teams require a `team.yaml` to be
/// loadable from `~/.ccteam/teams/<team>/team.yaml`. The dev team is
/// grandfathered in (no team.yaml required) so legacy installs
/// without `ccteam init` still work. Meta-agent is also allowed
/// without a team.yaml — its bootstrap path is bespoke.
///
/// V0.2 §6.4 candidate 3: shipped seeds (dev / product-research /
/// meta-agent) are stamped to disk inside `run_new` so a fresh
/// install no longer needs an explicit `ccteam init` for the
/// validation to find them.
pub fn run_new(paths: &CcteamPaths, request: &str, team: &str) -> Result<String> {
    if request.trim().is_empty() {
        bail!("ccteam new: request must be non-empty");
    }
    if team.trim().is_empty() {
        bail!("ccteam new: --team must be non-empty");
    }
    // Self-heal shipped seeds before validating — without this, a
    // first-time `ccteam new --team product-research` against a
    // freshly-installed binary would fail with "unknown team".
    // force=false preserves operator hand-edits.
    if let Err(err) = ccteam_core::write_all_global_team_templates(&paths.root, false) {
        tracing::warn!(
            error = %err,
            root = %paths.root.display(),
            "ccteam new: could not seed shipped team templates",
        );
    }
    ensure_team_resolvable(paths, team)?;
    let slug = pick_unused_slug(paths, request, team)?;
    bootstrap_project(paths, &slug, request, team)?;
    Ok(slug)
}

/// Resolve `team` against the on-disk team registry. Returns
/// `Ok(())` when the team is bootable; otherwise returns a
/// fail-fast error pointing the user at the missing team.yaml.
///
/// Resolution order:
/// 1. `dev` and `meta-agent` always succeed (legacy / bespoke paths).
/// 2. If `~/.ccteam/teams/<team>/team.yaml` is on disk, load + validate it.
/// 3. Otherwise fail with a help message listing the teams currently
///    on disk.
fn ensure_team_resolvable(paths: &CcteamPaths, team: &str) -> Result<()> {
    if team == "dev" || team == ccteam_core::META_TEAM_NAME {
        return Ok(());
    }
    let yaml_path = paths.root.join("teams").join(team).join("team.yaml");
    if yaml_path.exists() {
        TeamSpec::load(&yaml_path).with_context(|| {
            format!("ccteam new: failed to load {}", yaml_path.display())
        })?;
        return Ok(());
    }
    let known = list_disk_teams(paths)
        .ok()
        .filter(|t| !t.is_empty())
        .map(|t| t.join(", "))
        .unwrap_or_else(|| "(none yet — run `ccteam doctor --reset-shipped-teams`)".into());
    bail!(
        "ccteam new: unknown team `{team}` — \
         create {} (see docs/interfaces.md §5.5 for schema), \
         then re-run.\n\
         Teams currently on disk: {known}.",
        yaml_path.display(),
    )
}

/// Enumerate teams discoverable under `<global_dir>/teams/<name>/team.yaml`.
/// Used for error messages so users see what is actually available
/// (V0.2 §6.4 candidate 3 — disk-driven team registry).
fn list_disk_teams(paths: &CcteamPaths) -> Result<Vec<String>> {
    let teams_dir = paths.root.join("teams");
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(&teams_dir) {
        Ok(e) => e,
        Err(_) => return Ok(out),
    };
    for entry in entries.flatten() {
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false)
            && entry.path().join("team.yaml").exists()
        {
            if let Some(name) = entry.file_name().to_str().map(String::from) {
                out.push(name);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// `ccteam ls`. Returns either a human table or the interfaces.md §10.3
/// JSON shape (a single string, not printed — caller decides).
pub fn run_ls(paths: &CcteamPaths, format: OutputFormat) -> Result<String> {
    let projects = collect_projects(paths)?;
    Ok(match format {
        OutputFormat::Text => render_ls_text(&projects),
        OutputFormat::Json => render_ls_json(&projects)?,
    })
}

/// `ccteam show <slug>`. Renders the full project view per
/// interfaces.md §10.3 (json) or a human-readable summary (text).
pub fn run_show(paths: &CcteamPaths, slug: &str, format: OutputFormat) -> Result<String> {
    let state_path = paths.project_state(slug);
    if !state_path.exists() {
        bail!("project not found: {slug}");
    }
    let state = ProjectState::load(&state_path)?;
    let recent = collect_recent_events(paths, slug, 50)?;
    let artifacts = collect_artifacts(paths, slug);

    Ok(match format {
        OutputFormat::Text => render_show_text(&state, &recent, &artifacts),
        OutputFormat::Json => render_show_json(&state, &recent, &artifacts)?,
    })
}

/// `ccteam attach <slug>`. Execs `tmux attach -t ccteam-<slug>`. Exits
/// successfully when the user detaches; non-zero on error.
pub fn run_attach(slug: &str) -> Result<()> {
    let session = TmuxSession::for_slug(slug);
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

/// `ccteam peek <slug>`. Returns the contents of the session's first
/// pane via `tmux capture-pane -p`.
pub fn run_peek(slug: &str) -> Result<String> {
    let session = TmuxSession::for_slug(slug);
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
    let path = paths.progress_jsonl(slug);
    if !path.exists() {
        bail!("no progress.jsonl yet for {slug}: {}", path.display());
    }
    let mut stdout = std::io::stdout().lock();
    let initial = std::fs::read_to_string(&path)
        .with_context(|| format!("read {}", path.display()))?;
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
pub fn run_resume(paths: &CcteamPaths, slug: &str) -> Result<()> {
    let state_path = paths.project_state(slug);
    let mut state = ProjectState::load(&state_path)
        .with_context(|| format!("load state for {slug}"))?;
    state.user_pause_pending = false;
    state.user_attached = false;
    state.phase_state = PhaseState::Idle;
    state.last_user_interaction_at = Utc::now();
    if matches!(
        state.phase_history.last().map(|h| h.status.as_str()),
        Some("escalated"),
    ) {
        state.phase_history.push(PhaseHistoryEntry {
            phase: state.current_phase.clone(),
            status: "resumed".into(),
            duration_s: 0,
            cost_usd: 0.0,
        });
    }
    state.save(&state_path)?;

    let esc = paths.project_ccteam_dir(slug).join("escalation.md");
    if esc.exists() {
        let archive = paths
            .project_ccteam_dir(slug)
            .join(format!("escalation.{}.md", state.context_reset_count));
        let _ = std::fs::rename(&esc, &archive);
    }
    Ok(())
}

/// One row in the cross-project decisions queue (`ccteam decisions`).
///
/// A "decision" = an outbox file in any project's `.ccteam/outbox/` whose
/// front matter has `event_kind: clarify` or `event_kind: escalation`.
/// These are the messages a user must look at; everything else (replies,
/// progress, shipped) is informational and excluded.
#[derive(Debug, Clone)]
pub struct DecisionRow {
    pub slug: String,
    pub current_phase: String,
    pub team: String,
    pub event_kind: OutboxEventKind,
    pub priority: ccteam_core::OutboxPriority,
    pub created_at: chrono::DateTime<Utc>,
    pub age_seconds: u64,
    pub outbox_filename: String,
    pub summary: String,
}

/// `ccteam decisions`. Aggregate the cross-project decisions queue and
/// render it. interfaces.md §5.6.4 — meta-agent uses this as the
/// surface for "你有 N 条待决策" UX. M1 follow-up increment.
pub fn run_decisions(paths: &CcteamPaths, format: OutputFormat) -> Result<String> {
    let rows = collect_decisions(paths)?;
    Ok(match format {
        OutputFormat::Text => render_decisions_text(&rows),
        OutputFormat::Json => render_decisions_json(&rows)?,
    })
}

/// Walk every project under `projects_root`, scan their outbox dirs, and
/// emit one `DecisionRow` per outbox file whose `event_kind` warrants
/// user attention. Broken outbox files are warned and skipped — never
/// fatal, because one malformed file shouldn't blank the whole queue.
pub fn collect_decisions(paths: &CcteamPaths) -> Result<Vec<DecisionRow>> {
    let dir = &paths.projects_root;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let now = Utc::now();
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)
        .with_context(|| format!("read_dir {}", dir.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(slug) = entry.file_name().to_str().map(String::from) else {
            continue;
        };
        let project_dir = entry.path();
        let ccteam_dir = project_dir.join(".ccteam");
        if !ccteam_dir.exists() {
            continue;
        }
        // Resolve current phase + team from state.json. Missing /
        // unparseable state.json is *not* a decisions-queue error; the
        // outbox can still be surfaced under "<unknown>".
        let (current_phase, team) = match ProjectState::load(&paths.project_state(&slug)) {
            Ok(s) => {
                let phase = if s.current_phase.is_empty() {
                    "<idle>".to_string()
                } else {
                    s.current_phase
                };
                (phase, s.team)
            }
            Err(_) => ("<unknown>".to_string(), "<unknown>".to_string()),
        };

        let mailbox = SessionMailbox::for_ccteam_dir(&ccteam_dir);
        let files = match mailbox.list_outbox() {
            Ok(f) => f,
            Err(err) => {
                tracing::warn!(slug, error = %err, "skip project: outbox listing failed");
                continue;
            }
        };
        for path in files {
            let msg = match OutboxMessage::load(&path) {
                Ok(m) => m,
                Err(err) => {
                    tracing::warn!(
                        slug,
                        path = %path.display(),
                        error = %err,
                        "skip outbox file: parse failed",
                    );
                    continue;
                }
            };
            // Filter: only items the user has to look at. Replies /
            // progress / shipped notifications are not "decisions".
            if !matches!(
                msg.front.event_kind,
                OutboxEventKind::Clarify | OutboxEventKind::Escalation
            ) {
                continue;
            }
            let age = now
                .signed_duration_since(msg.front.created_at)
                .num_seconds()
                .max(0) as u64;
            let outbox_filename = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            out.push(DecisionRow {
                slug: slug.clone(),
                current_phase: current_phase.clone(),
                team: team.clone(),
                event_kind: msg.front.event_kind,
                priority: msg.front.priority,
                created_at: msg.front.created_at,
                age_seconds: age,
                outbox_filename,
                summary: summarize_body(&msg.body),
            });
        }
    }
    // Highest priority first, then oldest first within priority — old
    // unanswered escalations should never get buried under a fresh
    // clarify.
    out.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| a.created_at.cmp(&b.created_at))
    });
    Ok(out)
}

/// Pull a one-line summary from the outbox body. Strips the first
/// markdown heading (if any), trims, and truncates to 80 chars.
fn summarize_body(body: &str) -> String {
    let line = body
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("# "))
        .unwrap_or("");
    if line.chars().count() <= 80 {
        line.to_string()
    } else {
        let truncated: String = line.chars().take(77).collect();
        format!("{truncated}...")
    }
}

fn render_decisions_text(rows: &[DecisionRow]) -> String {
    if rows.is_empty() {
        return "no pending decisions across all projects.\n".to_string();
    }
    let mut out = String::new();
    out.push_str(&format!(
        "{} pending decision{} across all projects:\n\n",
        rows.len(),
        if rows.len() == 1 { "" } else { "s" },
    ));
    out.push_str("slug                       phase             team             kind         age      summary\n");
    out.push_str("------------------------   ---------------   --------------   ----------   ------   -------\n");
    for row in rows {
        let age = format_age(row.age_seconds);
        let kind = match row.event_kind {
            OutboxEventKind::Clarify => "clarify",
            OutboxEventKind::Escalation => "escalation",
            // Filtered upstream; defensive default.
            _ => "other",
        };
        out.push_str(&format!(
            "{:<26} {:<17} {:<16} {:<12} {:<8} {}\n",
            ellipsize(&row.slug, 26),
            ellipsize(&row.current_phase, 17),
            ellipsize(&row.team, 16),
            kind,
            age,
            row.summary,
        ));
    }
    out
}

fn render_decisions_json(rows: &[DecisionRow]) -> Result<String> {
    let arr: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "slug": row.slug,
                "current_phase": row.current_phase,
                "team": row.team,
                "event_kind": match row.event_kind {
                    OutboxEventKind::Clarify => "clarify",
                    OutboxEventKind::Escalation => "escalation",
                    OutboxEventKind::Reply => "reply",
                    OutboxEventKind::Progress => "progress",
                    OutboxEventKind::Shipped => "shipped",
                },
                "priority": match row.priority {
                    ccteam_core::OutboxPriority::Normal => "normal",
                    ccteam_core::OutboxPriority::High => "high",
                },
                "created_at": row.created_at.to_rfc3339(),
                "age_seconds": row.age_seconds,
                "outbox_filename": row.outbox_filename,
                "summary": row.summary,
            })
        })
        .collect();
    let body = json!({
        "total": rows.len(),
        "decisions": arr,
    });
    Ok(serde_json::to_string_pretty(&body)? + "\n")
}

fn format_age(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86400 {
        format!("{}h", seconds / 3600)
    } else {
        format!("{}d", seconds / 86400)
    }
}

fn ellipsize(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{truncated}…")
    }
}

/// `ccteam doctor` flags. Each mode is a separate boolean / option so
/// they can be combined (e.g. `--install-meta-agent rob` implies
/// `--install-skill` automatically).
#[derive(Debug, Clone, Default)]
pub struct DoctorOptions {
    pub install_recommended_agents: bool,
    pub dry_run: bool,
    pub force: bool,
    pub tool_surface: bool,
    /// M1.8: install ~/.claude/skills/ccteam-control/SKILL.md.
    pub install_skill: bool,
    /// M1.0: bootstrap a meta-agent project for the given user handle.
    /// `Some("rob")` ⇒ creates `~/projects/rob-meta/` and triggers
    /// `install_skill` regardless of its standalone flag.
    pub install_meta_agent: Option<String>,
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
}

/// `ccteam doctor` dispatch. Returns a human-readable report so unit
/// tests don't need to capture stdout.
pub fn run_doctor(paths: &CcteamPaths, opts: DoctorOptions) -> Result<String> {
    let any_mode = opts.install_recommended_agents
        || opts.tool_surface
        || opts.install_skill
        || opts.install_meta_agent.is_some()
        || opts.install_mcp
        || opts.install_memory_bridge
        || opts.reset_shipped_teams;
    if !any_mode {
        return Ok(String::from(
            "ccteam doctor: pass at least one mode flag.\n\
             \n\
             modes:\n  \
             --install-recommended-agents [--dry-run] [--force]\n      \
             ln -sf 8 plugin agents into ~/.claude/agents/ (M0.5.5).\n  \
             --tool-surface\n      \
             cross-check phase templates' tools_required against current reachability (M0.5.6).\n  \
             --install-skill [--force]\n      \
             write ~/.claude/skills/ccteam-control/SKILL.md (M1.8).\n  \
             --install-meta-agent <user-handle>\n      \
             bootstrap a meta-agent project for the given user (M1.0). Implies --install-skill.\n  \
             --install-mcp\n      \
             register `mcpServers.ccteam` in ~/.claude.json so daily-driver claude + meta-agent see the 9-tool MCP server (M2.5).\n  \
             --install-memory-bridge [--dry-run]\n      \
             write ~/.claude/rules/ccteam-lessons-<team>.md placeholders for every team with non-empty retro_schema (M4.2; V0.2 M0.16.2 — disk-driven team discovery).\n  \
             --reset-shipped-teams [--force]\n      \
             re-seed shipped team templates (~/.ccteam/teams/<name>/team.yaml + ~/.ccteam/<phase_dir>/*.md) from the in-binary bundle. Without --force, operator hand-edits are preserved; with --force, every shipped file is overwritten (V0.2 M0.16.2).\n",
        ));
    }
    let mut out = String::new();
    if opts.install_recommended_agents {
        out.push_str(&render_install_recommended_agents_report(&opts)?);
    }
    if opts.tool_surface {
        out.push_str(&render_tool_surface_report(paths)?);
    }
    // --install-meta-agent implies --install-skill so a fresh meta
    // session has the dispatcher tool list immediately.
    let install_skill_now = opts.install_skill || opts.install_meta_agent.is_some();
    if install_skill_now {
        out.push_str(&render_install_skill_report(&opts)?);
    }
    if let Some(handle) = &opts.install_meta_agent {
        out.push_str(&render_install_meta_agent_report(paths, handle)?);
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
    Ok(out)
}

fn render_reset_shipped_teams_report(
    paths: &CcteamPaths,
    opts: &DoctorOptions,
) -> Result<String> {
    ccteam_core::write_all_global_team_templates(&paths.root, opts.force)?;
    let mut out = String::from("ccteam doctor --reset-shipped-teams\n\n");
    if opts.force {
        out.push_str(
            "  Re-wrote every shipped team's team.yaml + phase markdowns under \
             ~/.ccteam/ (--force overwrote operator hand-edits).\n",
        );
    } else {
        out.push_str(
            "  Seeded shipped teams under ~/.ccteam/ (skipped existing files; \
             pass --force to overwrite operator hand-edits).\n",
        );
    }
    Ok(out)
}

fn render_install_memory_bridge_report(paths: &CcteamPaths, opts: &DoctorOptions) -> Result<String> {
    // V0.2 §6.4 candidate 3: bridge teams are now discovered by
    // scanning `~/.ccteam/teams/<name>/team.yaml`. Make sure shipped
    // seeds are present before the scan so a fresh install (no
    // `ccteam init` yet) still lays down dev / product-research
    // bridges. force=false preserves operator hand-edits.
    if !opts.dry_run {
        ccteam_core::write_all_global_team_templates(&paths.root, false)?;
    }
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
    out.push_str(&format!("  registered ccteam MCP server in {}\n", path.display()));
    out.push_str("  tools surface : 9 (interfaces §12.2)\n");
    out.push_str("  consumers     : daily-driver claude + meta-agent\n");
    out.push('\n');
    out.push_str(
        "open a new claude session to pick up the change; existing sessions need /reload-mcp.\n",
    );
    Ok(out)
}

fn render_install_skill_report(opts: &DoctorOptions) -> Result<String> {
    let report = install_ccteam_control_skill(InstallSkillOptions {
        force: opts.force,
        dry_run: opts.dry_run,
    })?;
    let mut out = String::from("ccteam doctor --install-skill\n\n");
    let label: String = match &report.action {
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
    };
    out.push_str(&format!("  ccteam-control  {label}  {}\n", report.target.display()));
    out.push('\n');
    Ok(out)
}

fn render_install_meta_agent_report(paths: &CcteamPaths, user_handle: &str) -> Result<String> {
    let report: MetaBootstrapReport = bootstrap_meta_project(paths, user_handle)?;
    let mut out = String::from("ccteam doctor --install-meta-agent\n\n");
    out.push_str(&format!("  user handle      {user_handle}\n"));
    out.push_str(&format!("  project slug     {}\n", report.slug));
    out.push_str(&format!("  project dir      {}\n", report.project_dir.display()));
    out.push_str(&format!("  role prompt      {}\n", report.claude_md.display()));
    out.push_str(&format!(
        "  status           {}\n",
        if report.already_existed { "refreshed" } else { "created" },
    ));
    out.push('\n');
    out.push_str(&format!(
        "tmux session     ccteam-meta-{}\n",
        ccteam_core::meta_slug(user_handle)?.trim_end_matches("-meta"),
    ));
    out.push_str(&format!(
        "attach with      tmux attach -t ccteam-meta-{}\n",
        ccteam_core::meta_slug(user_handle)?.trim_end_matches("-meta"),
    ));
    out.push_str("\nrun `ccteam start --foreground` (in another terminal) to wake the meta session.\n");
    Ok(out)
}

fn render_install_recommended_agents_report(opts: &DoctorOptions) -> Result<String> {
    let reports = link_recommended_agents(LinkOptions {
        force: opts.force,
        dry_run: opts.dry_run,
    })?;
    let mut out = String::new();
    out.push_str(if opts.dry_run {
        "ccteam doctor --install-recommended-agents (dry-run)\n"
    } else {
        "ccteam doctor --install-recommended-agents\n"
    });
    out.push('\n');
    let mut all_ok = true;
    for r in &reports {
        out.push_str(&render_agent_link_line(r));
        if !r.action.is_ok() {
            all_ok = false;
        }
    }
    out.push('\n');
    if !all_ok {
        out.push_str(
            "some agents skipped — pass --force to overwrite user files, or install \
             claude-plugins-official for missing sources.\n",
        );
    } else if opts.dry_run {
        out.push_str(
            "no changes made (--dry-run). drop the flag to apply.\n",
        );
    } else {
        out.push_str("all 8 plugin agents reachable via Task(subagent_type=...)\n");
    }
    Ok(out)
}

fn render_agent_link_line(r: &AgentLinkReport) -> String {
    let action_label = format_action_label(&r.action);
    format!(
        "  {:<28} {:<28} {}\n",
        r.agent.filename, action_label, r.target.display(),
    )
}

fn format_action_label(action: &AgentLinkAction) -> String {
    use AgentLinkAction::*;
    match action {
        Linked => "linked".into(),
        AlreadyLinked => "already-linked".into(),
        Replaced { previous_target } => {
            format!("replaced (was -> {})", previous_target.display())
        }
        Kept { previous_target } => {
            format!("kept foreign link (was -> {}; use --force to replace)", previous_target.display())
        }
        SkippedUserFile => "skipped (user file)".into(),
        SkippedSourceMissing { source } => {
            format!("skipped (source missing: {})", source.display())
        }
        DryRun { would } => format!("would: {}", format_action_label(would)),
    }
}

fn render_tool_surface_report(paths: &CcteamPaths) -> Result<String> {
    let claude = user_claude_dir()?;
    let snap = ToolSurfaceSnapshot::scan(&claude)?;
    let templates = load_local_phase_templates(paths)?;

    let mut out = String::new();
    out.push_str("ccteam doctor --tool-surface\n");
    out.push_str(&format!(
        "\nclaude dir       : {}\n",
        claude.display()
    ));
    out.push_str(&format!(
        "phase templates  : {} loaded from {}\n",
        templates.len(),
        paths.phases_dir().display(),
    ));
    out.push_str(&format!(
        "subagents seen   : {} (incl. {} built-ins)\n",
        snap.subagents.len(),
        BUILTIN_SUBAGENTS.len(),
    ));
    out.push_str(&format!("skills seen      : {}\n", snap.skills.len()));
    out.push_str(&format!("mcp servers seen : {}\n", snap.mcp.len()));
    out.push('\n');

    if templates.is_empty() {
        out.push_str(
            "no phase templates under ~/.ccteam/phases/ — run `ccteam init` first.\n",
        );
        return Ok(out);
    }

    out.push_str("| phase | kind | name | status | fix |\n");
    out.push_str("|---|---|---|---|---|\n");
    let mut any_missing = false;
    for t in &templates {
        let req = &t.tools_required;
        let mut emitted = false;
        for kind_name in [
            ("subagent", &req.subagents, &snap.subagents),
            ("skill", &req.skills, &snap.skills),
            ("mcp", &req.mcp, &snap.mcp),
        ] {
            let (kind, list, set) = kind_name;
            for name in list {
                emitted = true;
                if set.contains(name) {
                    out.push_str(&format!(
                        "| {phase} | {kind} | `{name}` | OK | — |\n",
                        phase = t.name,
                    ));
                } else {
                    any_missing = true;
                    let m = match kind {
                        "subagent" => ccteam_core::MissingTool::Subagent(name.clone()),
                        "skill" => ccteam_core::MissingTool::Skill(name.clone()),
                        _ => ccteam_core::MissingTool::Mcp(name.clone()),
                    };
                    out.push_str(&format!(
                        "| {phase} | {kind} | `{name}` | **MISSING** | {fix} |\n",
                        phase = t.name,
                        fix = m.fix_hint(),
                    ));
                }
            }
        }
        if !emitted {
            out.push_str(&format!(
                "| {phase} | — | — | none required | — |\n",
                phase = t.name,
            ));
        }
    }

    out.push('\n');
    if any_missing {
        out.push_str(
            "**Verdict:** at least one phase has a missing tool. \
             `ccteam start` will refuse until they're installed (or pass --skip-tool-check).\n",
        );
    } else {
        out.push_str("**Verdict:** all phase tool dependencies reachable.\n");
    }
    Ok(out)
}

fn load_local_phase_templates(paths: &CcteamPaths) -> Result<Vec<PhaseTemplate>> {
    let dir = paths.phases_dir();
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .with_context(|| format!("read_dir {}", dir.display()))?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("md"))
        })
        .collect();
    entries.sort_by_key(|e| e.file_name());
    let mut out = Vec::with_capacity(entries.len());
    for e in entries {
        let path = e.path();
        out.push(
            PhaseTemplate::load(&path)
                .with_context(|| format!("load {}", path.display()))?,
        );
    }
    Ok(out)
}

/// Project metadata with derived fields used by `ls`. Pulled out so
/// rendering and the JSON path share one source of truth.
#[derive(Debug)]
pub struct ProjectSummary {
    pub state: ProjectState,
    pub age_seconds: u64,
    pub stall_silent_seconds: u64,
}

pub fn collect_projects(paths: &CcteamPaths) -> Result<Vec<ProjectSummary>> {
    let dir = &paths.projects_root;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("read_dir {}", dir.display()))? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(slug) = entry.file_name().to_str().map(String::from) else {
            continue;
        };
        let state_path = paths.project_state(&slug);
        if !state_path.exists() {
            continue;
        }
        let state = match ProjectState::load(&state_path) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(slug, error = %err, "skip project: state.json failed to load");
                continue;
            }
        };
        let now = Utc::now();
        let age = now
            .signed_duration_since(state.created_at)
            .num_seconds()
            .max(0) as u64;
        let silent = state
            .last_progress_event_at
            .map(|t| now.signed_duration_since(t).num_seconds().max(0) as u64)
            .unwrap_or(age);
        out.push(ProjectSummary {
            state,
            age_seconds: age,
            stall_silent_seconds: silent,
        });
    }
    out.sort_by(|a, b| a.state.slug.cmp(&b.state.slug));
    Ok(out)
}

pub fn collect_recent_events(
    paths: &CcteamPaths,
    slug: &str,
    n: usize,
) -> Result<Vec<Value>> {
    let path = paths.progress_jsonl(slug);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let body = std::fs::read_to_string(&path)
        .with_context(|| format!("read {}", path.display()))?;
    let mut all: Vec<Value> = body
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    if all.len() > n {
        let drop = all.len() - n;
        all.drain(..drop);
    }
    Ok(all)
}

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

fn render_ls_text(projects: &[ProjectSummary]) -> String {
    if projects.is_empty() {
        return "(no projects under ~/projects/. start one with `ccteam new \"<request>\"`.)\n".into();
    }
    let mut out = String::new();
    out.push_str("SLUG                                     PHASE          STATE       COST   AGE\n");
    for p in projects {
        let phase = display_phase(&p.state.current_phase);
        out.push_str(&format!(
            "{:<40} {:<14} {:<11} ${:<5.2} {}s\n",
            truncate(&p.state.slug, 40),
            truncate(phase, 14),
            phase_state_str(&p.state.phase_state),
            p.state.cost_used_usd,
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

fn render_ls_json(projects: &[ProjectSummary]) -> Result<String> {
    let active_count = projects
        .iter()
        .filter(|p| p.state.phase_state == PhaseState::InFlight)
        .count();
    let arr: Vec<Value> = projects
        .iter()
        .map(|p| {
            json!({
                "slug": p.state.slug,
                "current_phase": p.state.current_phase,
                "phase_state": phase_state_str(&p.state.phase_state),
                "cost_used_usd": p.state.cost_used_usd,
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
        "orchestrator": {
            "running": null,
            "active_count": active_count,
            "max_concurrent": 1,
        }
    });
    Ok(serde_json::to_string_pretty(&v)?)
}

fn render_show_text(
    state: &ProjectState,
    recent: &[Value],
    artifacts: &Map<String, Value>,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {} ({})\n\n", state.slug, state.tmux_session));
    out.push_str(&format!("current phase  : {}\n", display_phase(&state.current_phase)));
    out.push_str(&format!(
        "phase state    : {}\n",
        phase_state_str(&state.phase_state)
    ));
    out.push_str(&format!("cost used      : ${:.2}\n", state.cost_used_usd));
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
            out.push_str(&format!(
                "  {:<18} {}\n",
                k,
                v.as_str().unwrap_or("<?>")
            ));
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
    recent: &[Value],
    artifacts: &Map<String, Value>,
) -> Result<String> {
    let v = json!({
        "state": serde_json::to_value(state)?,
        "phase_history": serde_json::to_value(&state.phase_history)?,
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
        PhaseState::InFlight => "in_flight",
        PhaseState::Idle => "idle",
        PhaseState::AutoLocked => "fix_locked",
        PhaseState::DonePending { .. } => "done_pending",
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

    #[test]
    fn run_new_creates_slug_and_bootstrap_files() {
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let slug = run_new(&paths, "Build a bookmark manager", "dev").unwrap();
        // F22: slug now carries the team prefix so ~/.claude/rules/ccteam-lessons-dev.md
        // `paths: ~/projects/dev-*` matches at session start.
        assert!(slug.starts_with("dev-build-a-bookmark-manager"));
        let project = paths.project_dir(&slug);
        assert!(project.join(".ccteam/spec.md").exists());
        assert!(project.join(".ccteam/state.json").exists());
        assert!(project.join(".claude/settings.json").exists());
        assert!(project.join("CLAUDE.md").exists());
    }

    #[test]
    fn run_new_rejects_empty_request() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let err = run_new(&paths, "   \n\t", "dev").unwrap_err();
        assert!(format!("{err:#}").contains("non-empty"));
    }

    #[test]
    fn run_new_rejects_empty_team() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let err = run_new(&paths, "build something", "  ").unwrap_err();
        assert!(format!("{err:#}").contains("--team"));
    }

    #[test]
    fn run_new_records_team_in_state_json() {
        // M3.1 F12/F13: --team must persist into state.json so the
        // orchestrator can route this project's phase set.
        // V0.2 M0.16.2: non-dev teams require team.yaml on disk;
        // `run_new` self-heals shipped seeds first, so
        // `product-research`'s yaml lands at
        // ~/.ccteam/teams/product-research/team.yaml during this
        // call — no manual `ccteam init` needed.
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let slug =
            run_new(&paths, "ai recipe generator idea", "product-research").unwrap();
        let state = ProjectState::load(&paths.project_state(&slug)).unwrap();
        assert_eq!(state.team, "product-research");
    }

    #[test]
    fn run_new_rejects_unknown_team_with_helpful_error() {
        // M3.3 + V0.2 M0.16.2: unknown team (not in shipped seeds, not
        // on disk after self-heal) must fail-fast with a clear pointer
        // to ~/.ccteam/teams/<team>/team.yaml.
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let err = run_new(&paths, "marketing copy idea", "marketing").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("unknown team"), "got: {msg}");
        assert!(msg.contains("marketing"), "should name the missing team");
        assert!(
            msg.contains("teams/marketing/team.yaml"),
            "should point at the missing path",
        );
        // After M0.16.2 self-heal, the disk-driven team list includes
        // the shipped seeds — the error message lists those so users
        // see the catalog of options:
        assert!(msg.contains("dev"));
        assert!(msg.contains("product-research"));
    }

    #[test]
    fn run_new_accepts_team_yaml_on_disk_for_user_team() {
        // M3.3: a user-authored team (not shipped) becomes valid as
        // soon as ~/.ccteam/teams/<team>/team.yaml is on disk. The
        // fail-fast check loads + validates the YAML.
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let yaml_path = paths
            .root
            .join("teams")
            .join("custom-team")
            .join("team.yaml");
        std::fs::create_dir_all(yaml_path.parent().unwrap()).unwrap();
        std::fs::write(
            &yaml_path,
            "name: custom-team\nphase_dir: phases-custom\n",
        )
        .unwrap();
        // run_new without embedded phases for `custom-team` will fail
        // later in bootstrap_project (no template bundle), but
        // ensure_team_resolvable should accept the team.yaml first.
        // The error we get back should NOT mention "unknown team".
        let err = run_new(&paths, "do a thing", "custom-team").unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            !msg.contains("unknown team"),
            "team.yaml on disk should pass the resolve check; got: {msg}",
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
        run_new(&paths, "demo one", "dev").unwrap();
        run_new(&paths, "demo two", "dev").unwrap();

        let body = run_ls(&paths, OutputFormat::Json).unwrap();
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["projects"].as_array().unwrap().len(), 2);
        assert_eq!(v["orchestrator"]["active_count"], 0);
        assert_eq!(v["orchestrator"]["max_concurrent"], 1);
    }

    #[test]
    fn run_show_json_includes_state_and_artifacts() {
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let slug = run_new(&paths, "demo", "dev").unwrap();
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
        let slug = run_new(&paths, "demo", "dev").unwrap();
        // simulate an escalated state
        let state_path = paths.project_state(&slug);
        let mut state = ProjectState::load(&state_path).unwrap();
        state.phase_state = PhaseState::InFlight;
        state.user_pause_pending = true;
        state.save(&state_path).unwrap();
        let esc = paths.project_ccteam_dir(&slug).join("escalation.md");
        std::fs::write(&esc, "stuck").unwrap();

        run_resume(&paths, &slug).unwrap();
        let s2 = ProjectState::load(&state_path).unwrap();
        assert_eq!(s2.phase_state, PhaseState::Idle);
        assert!(!s2.user_pause_pending);
        assert!(!esc.exists(), "escalation.md should be archived after resume");
    }

    #[test]
    fn run_resume_appends_resumed_history_after_escalated_entry() {
        // E2E 2026-05-06 F8: an escalated `phase_history` entry leaves
        // `Dag::is_terminal_state` permanently true. `ccteam resume`
        // must append a paired `"resumed"` entry so future ticks
        // dispatch again. Append-only — the original escalated entry
        // stays intact for audit.
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let slug = run_new(&paths, "demo", "dev").unwrap();

        let state_path = paths.project_state(&slug);
        let mut state = ProjectState::load(&state_path).unwrap();
        state.phase_state = PhaseState::InFlight;
        state.current_phase = "fix".into();
        state.phase_history.push(PhaseHistoryEntry {
            phase: "fix".into(),
            status: "escalated".into(),
            duration_s: 0,
            cost_usd: 0.0,
        });
        state.save(&state_path).unwrap();

        run_resume(&paths, &slug).unwrap();
        let s2 = ProjectState::load(&state_path).unwrap();
        assert_eq!(s2.phase_history.len(), 2, "resume must append, not mutate");
        assert_eq!(s2.phase_history[0].status, "escalated");
        assert_eq!(s2.phase_history[1].status, "resumed");
        assert_eq!(s2.phase_history[1].phase, "fix");
    }

    #[test]
    fn run_resume_does_not_append_resumed_when_no_escalation() {
        // Non-escalated resume (e.g. user attached + paused) must not
        // pollute phase_history with a spurious resumed entry — that
        // would keep growing on repeat resumes for benign pauses.
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let slug = run_new(&paths, "demo", "dev").unwrap();

        let state_path = paths.project_state(&slug);
        let mut state = ProjectState::load(&state_path).unwrap();
        state.user_pause_pending = true;
        state.phase_history.push(PhaseHistoryEntry {
            phase: "implement".into(),
            status: "passed".into(),
            duration_s: 0,
            cost_usd: 0.0,
        });
        state.save(&state_path).unwrap();

        run_resume(&paths, &slug).unwrap();
        let s2 = ProjectState::load(&state_path).unwrap();
        assert_eq!(s2.phase_history.len(), 1);
        assert_eq!(s2.phase_history[0].status, "passed");
    }

    #[test]
    fn run_init_creates_global_skeleton_and_unpacks_phases() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let report = run_init(&paths, InitOptions::default()).unwrap();
        for sub in ["phases", "templates", "progress", "inbox", "control"] {
            assert!(
                paths.root.join(sub).is_dir(),
                "init must create {}",
                paths.root.join(sub).display()
            );
        }
        assert!(paths.phases_dir().join("02-plan-eng.md").is_file());
        assert!(paths.phases_dir().join("09-ship.md").is_file());
        // M2.4: helper templates land alongside phase templates.
        assert!(paths
            .templates_dir()
            .join("review-with-user-loop.md")
            .is_file());
        assert!(paths
            .templates_dir()
            .join("kickoff-reverse-interview.md")
            .is_file());
        assert!(report.contains("phase templates"));
        assert!(report.contains("helper templates"));
        assert!(report.contains("next"));
    }

    #[test]
    fn run_init_is_idempotent_and_preserves_user_edits() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        run_init(&paths, InitOptions::default()).unwrap();
        let path = paths.phases_dir().join("02-plan-eng.md");
        std::fs::write(&path, "USER EDIT").unwrap();
        run_init(&paths, InitOptions::default()).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "USER EDIT");
        run_init(&paths, InitOptions { force: true }).unwrap();
        assert_ne!(std::fs::read_to_string(&path).unwrap(), "USER EDIT");
    }

    #[test]
    fn run_progress_emits_existing_events_without_tail() {
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let slug = run_new(&paths, "demo", "dev").unwrap();
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
        assert!(body.contains("install-recommended-agents"));
        assert!(body.contains("tool-surface"));
        assert!(body.contains("install-skill"));
        assert!(body.contains("install-meta-agent"));
        assert!(body.contains("install-memory-bridge"));
    }

    #[test]
    fn run_doctor_install_memory_bridge_writes_both_team_files() {
        ensure_isolation();
        let _guard = env_lock().lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        std::env::set_var("CLAUDE_CONFIG_HOME", tmp.path().to_str().unwrap());

        let opts = DoctorOptions {
            install_memory_bridge: true,
            ..DoctorOptions::default()
        };
        let report = run_doctor(&paths, opts).unwrap();
        assert!(report.contains("install-memory-bridge"));
        assert!(report.contains("dev"));
        assert!(report.contains("product-research"));

        let dev_path = tmp.path().join("rules/ccteam-lessons-dev.md");
        let pr_path = tmp.path().join("rules/ccteam-lessons-product-research.md");
        assert!(dev_path.is_file(), "dev lessons not written");
        assert!(pr_path.is_file(), "product-research lessons not written");

        std::env::remove_var("CLAUDE_CONFIG_HOME");
    }

    #[test]
    fn run_doctor_install_meta_agent_creates_project_and_skill() {
        // M1.0 + M1.8 combo: --install-meta-agent <handle> implies
        // --install-skill, so a single invocation gets the user a
        // ready-to-attach session.
        ensure_isolation();
        let _guard = env_lock().lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        // Redirect ~/.claude/ to the tempdir so the skill install
        // doesn't touch the developer's real ~/.claude/.
        std::env::set_var("CLAUDE_CONFIG_HOME", tmp.path().to_str().unwrap());

        let opts = DoctorOptions {
            install_meta_agent: Some("rob".into()),
            ..DoctorOptions::default()
        };
        let report = run_doctor(&paths, opts).unwrap();
        assert!(report.contains("install-skill"), "skill install report missing");
        assert!(report.contains("install-meta-agent"), "meta install report missing");
        assert!(report.contains("rob-meta"), "meta slug should be reported");

        // Project directory exists.
        assert!(paths.project_dir("rob-meta").is_dir());
        let state = ProjectState::load(&paths.project_state("rob-meta")).unwrap();
        assert_eq!(state.team, "meta-agent");
        assert_eq!(state.tmux_session, "ccteam-meta-rob");

        // Skill landed under the redirected ~/.claude/.
        let skill_path = tmp.path().join("skills/ccteam-control/SKILL.md");
        assert!(skill_path.is_file(), "skill SKILL.md not written: {}", skill_path.display());

        std::env::remove_var("CLAUDE_CONFIG_HOME");
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

        let skill_path = tmp.path().join("skills/ccteam-control/SKILL.md");
        assert!(skill_path.is_file());
        std::env::remove_var("CLAUDE_CONFIG_HOME");
    }

    #[test]
    fn format_action_label_renders_dry_run_clearly() {
        let label = format_action_label(&AgentLinkAction::DryRun {
            would: Box::new(AgentLinkAction::Linked),
        });
        assert_eq!(label, "would: linked");
    }

    // ---------------- V0.2 M0.16.2: shipped-team seed regen ----------------

    #[test]
    fn run_doctor_reset_shipped_teams_seeds_all_teams_under_global_dir() {
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let opts = DoctorOptions {
            reset_shipped_teams: true,
            ..DoctorOptions::default()
        };
        let report = run_doctor(&paths, opts).unwrap();
        assert!(report.contains("reset-shipped-teams"));
        // Every shipped team's team.yaml lands on disk under the
        // declared layout. (Layout migration to teams/<name>/ is
        // M0.17; for now teams/<name>/team.yaml is what
        // write_all_global_team_templates writes.)
        for team in ["dev", "product-research", "meta-agent"] {
            let yaml = paths.root.join("teams").join(team).join("team.yaml");
            assert!(
                yaml.is_file(),
                "shipped team `{team}` not seeded at {}",
                yaml.display(),
            );
        }
    }

    #[test]
    fn run_doctor_reset_shipped_teams_preserves_operator_edits_without_force() {
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);

        // First seed lays down the shipped dev/team.yaml. Tamper with
        // it the way an operator would.
        run_doctor(&paths, DoctorOptions {
            reset_shipped_teams: true,
            ..DoctorOptions::default()
        }).unwrap();
        let dev_yaml = paths.root.join("teams").join("dev").join("team.yaml");
        std::fs::write(&dev_yaml, "name: dev\ndescription: hand-edited\n").unwrap();

        // Re-run without --force; the hand-edit must survive.
        run_doctor(&paths, DoctorOptions {
            reset_shipped_teams: true,
            ..DoctorOptions::default()
        }).unwrap();
        let body = std::fs::read_to_string(&dev_yaml).unwrap();
        assert!(body.contains("hand-edited"), "operator edit clobbered without --force");

        // Now with --force, the seed wins again.
        run_doctor(&paths, DoctorOptions {
            reset_shipped_teams: true,
            force: true,
            ..DoctorOptions::default()
        }).unwrap();
        let body = std::fs::read_to_string(&dev_yaml).unwrap();
        assert!(body.contains("Software development team"));
        assert!(!body.contains("hand-edited"));
    }

    #[test]
    fn run_new_self_heals_shipped_seeds_for_first_time_install() {
        // V0.2 §6.4 candidate 3: a fresh install (no `ccteam init`) where
        // ~/.ccteam/teams/ is empty must still let `ccteam new --team
        // product-research` succeed because run_new self-heals.
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        assert!(!paths.root.join("teams").exists(), "precondition: empty global dir");

        let slug = run_new(&paths, "Verify product-research bootstraps", "product-research")
            .expect("run_new must auto-seed shipped templates before validation");
        assert!(slug.starts_with("product-research-"));
        // The seed must have landed during run_new.
        assert!(paths.root.join("teams/product-research/team.yaml").is_file());
    }

    // -------- ccteam decisions (M1 follow-up) --------

    /// Helper: write an outbox file with caller-controlled front matter
    /// to `<projects_root>/<slug>/.ccteam/outbox/<filename>`. Uses
    /// `OutboxMessage::save` so the file goes through the same atomic
    /// write path used in production.
    fn write_outbox(
        paths: &CcteamPaths,
        slug: &str,
        filename: &str,
        kind: OutboxEventKind,
        priority: ccteam_core::OutboxPriority,
        created_at: chrono::DateTime<Utc>,
        body: &str,
    ) {
        let dir = paths.project_ccteam_dir(slug).join("outbox");
        std::fs::create_dir_all(&dir).unwrap();
        let msg = OutboxMessage {
            front: ccteam_core::OutboxFrontMatter {
                schema_version: 1,
                in_reply_to: None,
                in_reply_to_source_msg_id: None,
                target_channels: Vec::new(),
                created_at,
                priority,
                event_kind: kind,
            },
            body: body.to_string(),
        };
        msg.save(&dir.join(filename)).unwrap();
    }

    fn write_state(paths: &CcteamPaths, slug: &str, phase: &str, team: &str) {
        let dir = paths.project_ccteam_dir(slug);
        std::fs::create_dir_all(&dir).unwrap();
        let mut state = ProjectState::initial_for_team(slug.to_string(), team.to_string());
        state.current_phase = phase.to_string();
        state.save(&paths.project_state(slug)).unwrap();
    }

    #[test]
    fn run_decisions_text_says_empty_when_no_projects() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let body = run_decisions(&paths, OutputFormat::Text).unwrap();
        assert!(body.contains("no pending decisions"));
    }

    #[test]
    fn run_decisions_excludes_reply_progress_shipped_event_kinds() {
        // Only clarify / escalation should show up in the queue.
        // Replies / progress / shipped notifications are informational,
        // not user-actionable.
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        write_state(&paths, "alpha", "plan-eng", "dev");
        let now = Utc::now();
        write_outbox(
            &paths,
            "alpha",
            "reply-1.md",
            OutboxEventKind::Reply,
            ccteam_core::OutboxPriority::Normal,
            now,
            "informational",
        );
        write_outbox(
            &paths,
            "alpha",
            "reply-2.md",
            OutboxEventKind::Progress,
            ccteam_core::OutboxPriority::Normal,
            now,
            "informational",
        );
        write_outbox(
            &paths,
            "alpha",
            "reply-3.md",
            OutboxEventKind::Shipped,
            ccteam_core::OutboxPriority::Normal,
            now,
            "informational",
        );
        let rows = collect_decisions(&paths).unwrap();
        assert!(rows.is_empty(), "non-decision event_kinds must be filtered out");
    }

    #[test]
    fn run_decisions_aggregates_clarify_and_escalation_across_projects() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        write_state(&paths, "alpha", "plan-eng", "dev");
        write_state(&paths, "beta", "kickoff", "product-research");
        let t1 = Utc::now() - chrono::Duration::hours(2);
        let t2 = Utc::now() - chrono::Duration::minutes(30);
        write_outbox(
            &paths,
            "alpha",
            "reply-1.md",
            OutboxEventKind::Clarify,
            ccteam_core::OutboxPriority::Normal,
            t1,
            "选 SQLite 还是 Postgres?",
        );
        write_outbox(
            &paths,
            "beta",
            "reply-1.md",
            OutboxEventKind::Escalation,
            ccteam_core::OutboxPriority::High,
            t2,
            "exceeded max_clarify_rounds",
        );
        let rows = collect_decisions(&paths).unwrap();
        assert_eq!(rows.len(), 2);
        // High-priority escalation must come first regardless of being
        // newer than the clarify.
        assert!(matches!(rows[0].event_kind, OutboxEventKind::Escalation));
        assert_eq!(rows[0].slug, "beta");
        assert_eq!(rows[1].slug, "alpha");
    }

    #[test]
    fn run_decisions_json_serializes_complete_row_shape() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        write_state(&paths, "alpha", "plan-eng", "dev");
        let opened = chrono::DateTime::parse_from_rfc3339("2026-05-06T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        write_outbox(
            &paths,
            "alpha",
            "reply-1.md",
            OutboxEventKind::Clarify,
            ccteam_core::OutboxPriority::Normal,
            opened,
            "需要确认部署目标",
        );
        let body = run_decisions(&paths, OutputFormat::Json).unwrap();
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["total"], 1);
        let row = &parsed["decisions"][0];
        assert_eq!(row["slug"], "alpha");
        assert_eq!(row["current_phase"], "plan-eng");
        assert_eq!(row["team"], "dev");
        assert_eq!(row["event_kind"], "clarify");
        assert_eq!(row["priority"], "normal");
        assert_eq!(row["outbox_filename"], "reply-1.md");
        assert!(row["summary"].as_str().unwrap().contains("部署"));
    }

    #[test]
    fn run_decisions_skips_unparseable_outbox_files() {
        // A malformed outbox file in one project must not blank the
        // queue — other projects should still surface.
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        write_state(&paths, "alpha", "plan-eng", "dev");
        write_state(&paths, "broken", "plan-eng", "dev");
        write_outbox(
            &paths,
            "alpha",
            "reply-1.md",
            OutboxEventKind::Clarify,
            ccteam_core::OutboxPriority::Normal,
            Utc::now(),
            "valid question",
        );
        // Hand-write garbage that doesn't parse as an outbox file.
        let bad_dir = paths.project_ccteam_dir("broken").join("outbox");
        std::fs::create_dir_all(&bad_dir).unwrap();
        std::fs::write(bad_dir.join("reply-bad.md"), "not even a yaml header").unwrap();

        let rows = collect_decisions(&paths).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].slug, "alpha");
    }

    #[test]
    fn run_decisions_summary_truncates_long_first_line() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        write_state(&paths, "alpha", "plan-eng", "dev");
        let long_line = "x".repeat(200);
        write_outbox(
            &paths,
            "alpha",
            "reply-1.md",
            OutboxEventKind::Clarify,
            ccteam_core::OutboxPriority::Normal,
            Utc::now(),
            &long_line,
        );
        let rows = collect_decisions(&paths).unwrap();
        assert_eq!(rows.len(), 1);
        let summary = &rows[0].summary;
        assert!(summary.ends_with("..."), "long summaries should be truncated with ellipsis");
        assert!(summary.chars().count() <= 80);
    }

    #[test]
    fn run_decisions_handles_project_without_state_json() {
        // A project dir with outbox files but no state.json (race during
        // bootstrap, manual cleanup, etc.) should still surface its
        // pending items rather than vanish silently.
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let dir = paths.project_ccteam_dir("orphan").join("outbox");
        std::fs::create_dir_all(&dir).unwrap();
        write_outbox(
            &paths,
            "orphan",
            "reply-1.md",
            OutboxEventKind::Escalation,
            ccteam_core::OutboxPriority::High,
            Utc::now(),
            "项目状态丢失但 outbox 还在",
        );
        let rows = collect_decisions(&paths).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].current_phase, "<unknown>");
        assert_eq!(rows[0].team, "<unknown>");
    }
}
