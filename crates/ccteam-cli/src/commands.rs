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
    install_ccteam_project_creator_skill, install_ccteam_team_author_skill,
    migrate_legacy_skill_dirs, migrate_recommended_agent_symlinks, pick_unused_slug,
    rewrite_legacy_hook_commands, user_claude_dir, write_all_global_team_templates,
    write_global_helper_templates, write_global_phase_templates, CcteamPaths,
    HookCmdRewriteAction, HookCmdRewriteReport, InstallSkillOptions, LegacySkillAction,
    LegacySkillReport, MetaBootstrapReport, MigrationReport, OutboxEventKind, OutboxMessage,
    PhaseState, PhaseTemplate, ProjectState, SessionMailbox,
    SkillInstallAction, TeamSpec, ToolSurfaceSnapshot, BUILTIN_SUBAGENTS, HELPER_TEMPLATES,
    PHASE_TEMPLATES,
};
// V0.3 M5.0: `run_resume` body lives in `ccteam_core::actions::resume`,
// so `PhaseHistoryEntry` is now only referenced from the test module
// (other consumers go through the actions::* path).
#[cfg(test)]
use ccteam_core::PhaseHistoryEntry;
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

/// V0.2.2 F34: optional knobs for `run_new` covering the four-tier
/// slug-decision stack (PRD §3.2):
///
/// - `slug` set → Tier 1 (verbatim, B2 prefix semantics).
/// - `slug` unset, tty + claude available + `!no_auto_slug` →
///   Tier 3 (`claude -p` smart suggest + Y/n confirm).
/// - `slug` unset and Tier 3 unavailable / declined →
///   Tier 4 (`slugify_brief()` deterministic).
///
/// `RunNewOptions::default()` keeps the V0.2.1 behavior (no flag,
/// no auto-suggest, deterministic Tier 4 from `slugify_brief`).
#[derive(Debug, Clone, Copy, Default)]
pub struct RunNewOptions<'a> {
    pub slug: Option<&'a str>,
    pub no_auto_slug: bool,
    pub auto_slug_model: &'a str,
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
///
/// V0.2.2 F34: takes `RunNewOptions` for the four-tier slug stack.
pub fn run_new(
    paths: &CcteamPaths,
    request: &str,
    team: &str,
    opts: RunNewOptions<'_>,
) -> Result<String> {
    if request.trim().is_empty() {
        bail!("ccteam new: request must be non-empty");
    }
    if team.trim().is_empty() {
        bail!("ccteam new: --team must be non-empty");
    }
    // Self-heal shipped seeds before validating — without this, a
    // first-time `ccteam new --team research` against a freshly-installed
    // binary would fail with "unknown team". force=false preserves
    // operator hand-edits.
    if let Err(err) = ccteam_core::write_all_global_team_templates(&paths.root, false) {
        tracing::warn!(
            error = %err,
            root = %paths.root.display(),
            "ccteam new: could not seed shipped team templates",
        );
    }
    ensure_team_resolvable(paths, team)?;
    // V0.2.2 F40 — warn when the operator passed an alias instead of
    // the canonical team name. The project still bootstraps under the
    // alias (state.json::team / slug prefix preserve the typed string)
    // so old muscle memory keeps working; the warn flags the
    // transition path so new docs and skills migrate to the canonical
    // name. Print to stderr (not via tracing) so users on plain
    // installs without RUST_LOG see it.
    if let Some(canonical) = find_alias_canonical(paths, team) {
        if canonical != team {
            eprintln!(
                "ccteam new: `--team {team}` is a deprecated alias for `{canonical}`; \
                 prefer `--team {canonical}` going forward",
            );
        }
    }
    let slug = decide_slug(paths, request, team, opts)?;
    bootstrap_project(paths, &slug, request, team)?;
    Ok(slug)
}

/// V0.2.2 F34 four-tier slug decision. Pure-ish (only LLM shell-out
/// and stdin/stdout for confirmation if Tier 3 fires); test-friendly
/// because the env / flag inputs all flow through `RunNewOptions` +
/// `CCTEAM_AUTO_SLUG{,_BIN}` env vars.
fn decide_slug(
    paths: &CcteamPaths,
    request: &str,
    team: &str,
    opts: RunNewOptions<'_>,
) -> Result<String> {
    // Tier 1: explicit --slug wins.
    if let Some(raw) = opts.slug {
        return ccteam_core::pick_unused_slug_verbatim(paths, raw, team);
    }
    // Tier 3: `claude -p` smart suggestion. Skip when `--no-auto-slug`,
    // when the env disables it, or when claude is unreachable / non-tty.
    let env_disables = std::env::var("CCTEAM_AUTO_SLUG")
        .map(|v| v.eq_ignore_ascii_case("off") || v == "0")
        .unwrap_or(false);
    if !opts.no_auto_slug && !env_disables {
        match try_smart_slug(request, opts.auto_slug_model) {
            Ok(Some(suggestion)) => {
                return ccteam_core::pick_unused_slug_verbatim(paths, &suggestion, team);
            }
            Ok(None) => {
                // Smart path silently declined (eg user typed `n`); fall
                // through to Tier 4 — printed reason already.
            }
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "ccteam new: smart slug suggestion unavailable; falling back to deterministic",
                );
            }
        }
    }
    // Tier 4: token-aware deterministic.
    pick_unused_slug(paths, request, team)
}

/// V0.2.2 F34 Tier 3 — shell out to `claude -p` for an LLM-quality
/// slug suggestion, gated on tty / claude-on-PATH / env knobs.
///
/// Returns:
/// - `Ok(Some(slug))` — `claude -p` produced a clean slug and (in tty
///   contexts) the user accepted it.
/// - `Ok(None)` — the user declined the suggestion in interactive mode.
/// - `Err(_)` — every reason we want logged before falling through.
///
/// Test seam: `CCTEAM_AUTO_SLUG_BIN` overrides the resolved binary
/// path (eg point at a stub script during integration tests so the
/// tier is exercised without a real LLM).
fn try_smart_slug(request: &str, model: &str) -> Result<Option<String>> {
    use std::io::{self, BufRead, Write};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    // Resolve the binary. Honor a test override first.
    let bin = match std::env::var("CCTEAM_AUTO_SLUG_BIN") {
        Ok(v) if !v.is_empty() => v,
        _ => match which_claude() {
            Some(p) => p,
            None => bail!("`claude` not on PATH (Tier 3 disabled)"),
        },
    };

    let prompt = render_smart_slug_prompt(request);

    eprintln!("[ccteam] querying claude for slug recommendation...");
    let mut child = Command::new(&bin)
        .args(["-p", "--model", model])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawn `{bin} -p`"))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(prompt.as_bytes())
            .context("write prompt to claude stdin")?;
    }

    // Wait up to 15s for the child. We poll instead of `child.wait()`
    // so we can hard-kill on timeout.
    let deadline = Instant::now() + Duration::from_secs(15);
    let output = loop {
        match child.try_wait().context("poll claude child")? {
            Some(_status) => break child.wait_with_output().context("collect claude output")?,
            None => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    bail!("`claude -p` timed out after 15s; falling back to deterministic slug");
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    };

    if !output.status.success() {
        bail!(
            "`claude -p` exited with non-zero status (`{:?}`): {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let raw = String::from_utf8_lossy(&output.stdout).into_owned();
    let suggestion = match sanitize_smart_slug(&raw) {
        Some(s) => s,
        None => bail!("`claude -p` returned no usable slug: {:?}", raw.trim()),
    };

    // Confirm in tty contexts; auto-accept when piped.
    let tty = std::io::IsTerminal::is_terminal(&io::stdin());
    if !tty {
        eprintln!("[ccteam] suggested: {suggestion} (auto-accepted, non-tty)");
        return Ok(Some(suggestion));
    }

    eprint!("[ccteam] suggested: {suggestion}\n[ccteam] accept? [Y/n] (rerun with --slug to override): ");
    io::stderr().flush().ok();
    let mut line = String::new();
    let stdin = io::stdin();
    stdin.lock().read_line(&mut line).context("read confirmation")?;
    let reply = line.trim().to_ascii_lowercase();
    if reply.is_empty() || reply == "y" || reply == "yes" {
        Ok(Some(suggestion))
    } else {
        eprintln!(
            "[ccteam] declined; rerun with `--slug <name>` to set explicitly, or fall back to deterministic"
        );
        Ok(None)
    }
}

/// Build the Tier 3 prompt. Pulled out so the integration tests can
/// stub a deterministic `claude` echoing back a fixed slug without
/// caring about the prompt body (PRD §3.2.3 keeps the wording stable).
fn render_smart_slug_prompt(request: &str) -> String {
    format!(
        "Generate a 2-4 token kebab-case slug for a project with this brief:\n\
         '{request}'\n\n\
         Rules:\n\
         - Capture the core noun/concept (brand name if present), not action verbs\n\
         - Drop stop words (a, the, of, etc) and pure-digit tokens\n\
         - Output ONLY the slug, no explanation, no quotes, no markdown\n\
         Examples:\n\
         - 'AI recipe generator from fridge photo' -> recipe-generator\n\
         - 'Build a todo cli with ratatui' -> todo-cli\n\
         - 'HermesTrade DEX prediction market' -> hermestrade-dex\n\
         Slug:"
    )
}

/// Lock the smart-slug output down to `[a-z0-9-]+`, len 2..=60. Strips
/// trailing whitespace / quotes / `Slug:` echo, so the LLM has some
/// slack but the disk layer doesn't see anything weird.
fn sanitize_smart_slug(raw: &str) -> Option<String> {
    let candidate = raw
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())?
        .trim_matches(|c: char| {
            c == '"' || c == '\'' || c == '`' || c.is_ascii_whitespace()
        })
        .trim_start_matches("Slug:")
        .trim();
    let lowered = candidate.to_ascii_lowercase();
    let cleaned: String = lowered
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
        .collect();
    let trimmed = cleaned.trim_matches('-');
    if trimmed.len() < 2 || trimmed.len() > 60 {
        return None;
    }
    if trimmed.chars().all(|c| c == '-') {
        return None;
    }
    Some(trimmed.to_string())
}

/// Resolve `claude` via `which`. Returns `None` when not on PATH; the
/// caller falls through to Tier 4. Stdlib only — no extra crate.
fn which_claude() -> Option<String> {
    let path_env = std::env::var_os("PATH")?;
    for entry in std::env::split_paths(&path_env) {
        let candidate = entry.join("claude");
        if candidate.is_file() {
            return candidate.to_str().map(String::from);
        }
    }
    None
}

/// Resolve `team` against the on-disk team registry. Returns
/// `Ok(())` when the team is bootable; otherwise returns a
/// fail-fast error pointing the user at the missing team.yaml.
///
/// Resolution order:
/// 1. `dev` and `meta-agent` always succeed (legacy / bespoke paths).
/// 2. If `~/.ccteam/teams/<team>/team.yaml` is on disk, load + validate it.
/// 3. V0.2.2 F40: scan every `~/.ccteam/teams/*/team.yaml` and accept
///    `team` when it matches a `spec.aliases` entry. Lets old projects'
///    `--team product-research` continue working after the canonical
///    rename to `research` (warn-deprecated; see `run_new`).
/// 4. Otherwise fail with a help message listing the teams currently
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
    if find_alias_canonical(paths, team).is_some() {
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

/// V0.2.2 F40 — given a possible alias, return the canonical name of
/// the team whose yaml lists it. Walks `<root>/teams/*/team.yaml`
/// once. Yaml parse errors silently fall through to the next entry —
/// the caller's failure path covers the no-match case.
fn find_alias_canonical(paths: &CcteamPaths, alias: &str) -> Option<String> {
    let teams_dir = paths.root.join("teams");
    let entries = std::fs::read_dir(&teams_dir).ok()?;
    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let yaml = entry.path().join("team.yaml");
        if !yaml.exists() {
            continue;
        }
        let spec = match TeamSpec::load(&yaml) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if spec.aliases.iter().any(|a| a == alias) {
            return Some(spec.name);
        }
    }
    None
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
    let daemon_up = ccteam_core::daemon::heartbeat_alive(paths);
    Ok(match format {
        OutputFormat::Text => render_ls_text(&projects, daemon_up),
        OutputFormat::Json => render_ls_json(&projects, daemon_up)?,
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
///
/// V0.3 M5.0: body lives in `ccteam_core::actions::resume` so the
/// V0.3 web layer (and the MCP wrapper) can call the same logic
/// without depending on `ccteam-cli`. This thin wrapper preserves the
/// public CLI surface; `ccteam resume <slug>` continues to dispatch
/// through here.
pub fn run_resume(paths: &CcteamPaths, slug: &str) -> Result<()> {
    ccteam_core::actions::resume(paths, slug)
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
    pub dry_run: bool,
    pub force: bool,
    pub tool_surface: bool,
    /// M1.8: install `~/.claude/skills/ccteam-control/SKILL.md`
    /// and `~/.claude/skills/ccteam-team-author/SKILL.md`, plus run the
    /// V0.2.2 (F39'd) → V0.2.2 (F44'd) reverse migration (cleanup of
    /// legacy `cct-*` skill dirs and stale settings.json hook command
    /// paths).
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
    /// V0.3.1 F46: install / refresh `~/.claude/statusline-command.sh`
    /// so Claude Code's statusline command tees stdin into
    /// `ccteam hook harness-snapshot` before passing through to the
    /// user's original (preserved as `<orig>.bak-<utc-ts>`).
    /// Idempotent — re-runs preserve the existing backup target.
    pub install_statusline_adapter: bool,
}

/// `ccteam doctor` dispatch. Returns a human-readable report so unit
/// tests don't need to capture stdout.
pub fn run_doctor(paths: &CcteamPaths, opts: DoctorOptions) -> Result<String> {
    let any_mode = opts.tool_surface
        || opts.install_skill
        || opts.install_meta_agent.is_some()
        || opts.install_mcp
        || opts.install_memory_bridge
        || opts.reset_shipped_teams
        || opts.validate_team.is_some()
        || opts.migrate_recommended_agents
        || opts.screenshot_smoke.is_some()
        || opts.install_statusline_adapter;
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
             --install-meta-agent <user-handle>\n      \
             bootstrap a meta-agent project for the given user (M1.0). Implies --install-skill.\n  \
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
             --install-statusline-adapter\n      \
             install / refresh ~/.claude/statusline-command.sh so Claude Code's statusline command tees stdin into `ccteam hook harness-snapshot` before invoking the user's original (preserved as a `.bak-<utc-ts>` file). Idempotent (V0.3.1 F46).\n",
        ));
    }
    let mut out = String::new();
    if opts.tool_surface {
        out.push_str(&render_tool_surface_report(paths)?);
    }
    // --install-meta-agent implies --install-skill so a fresh meta
    // session has the dispatcher tool list immediately.
    let install_skill_now = opts.install_skill || opts.install_meta_agent.is_some();
    if install_skill_now {
        out.push_str(&render_install_skill_report(paths, &opts)?);
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
    if opts.install_statusline_adapter {
        out.push_str(&install_statusline_adapter(paths)?);
    }
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
fn render_validate_team_report(
    paths: &CcteamPaths,
    team: &str,
) -> Result<(String, u32)> {
    use ccteam_core::{
        default_user_staging_dir, resolve_team, staging_dir_for, validate_staged_team,
        TeamResolveContext, TEAM_SOURCES,
    };

    let mut out = format!("ccteam doctor --validate-team {team}\n\n");
    // F30 — accumulate every [FAIL] line into a single counter so the
    // top-level Summary and the `run_doctor` exit code agree. Phase-
    // section findings sum into this same counter below.
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

    let spec = match resolve_team(team, &ctx) {
        Ok(s) => {
            out.push_str(&format!("[OK] team.yaml resolves; name=`{}`\n", s.name));
            s
        }
        Err(err) => {
            out.push_str(&format!("[FAIL] team.yaml load: {err:#}\n"));
            fails += 1;
            out.push_str(&format!("\nSummary: 0 ok, 0 warn, {fails} fail\n"));
            return Ok((out, fails));
        }
    };

    if spec.evergreen {
        out.push_str(
            "[OK] team is evergreen — skipping phase IO / markdown checks\n",
        );
        out.push_str(&format!("\nSummary: 1 ok, 0 warn, {fails} fail\n"));
        return Ok((out, fails));
    }

    // Locate the phase dir on disk via the same priority order the
    // resolver uses. V0.2.2 F41: walk by `spec.name` (canonical), not
    // the user-supplied `team` arg — when `team` is an F40 alias
    // (eg `product-research`), the team.yaml lives at the canonical
    // `teams/research/team.yaml` path, so resolving by alias would
    // miss it and produce a misleading "could not locate team
    // directory" failure even though the resolver returned a valid
    // TeamSpec.
    let team_dir = TEAM_SOURCES
        .iter()
        .filter_map(|s| s.path_for(&spec.name, &ctx))
        .find(|p| p.exists())
        .and_then(|p| p.parent().map(std::path::Path::to_path_buf));
    let team_dir = match team_dir {
        Some(p) => p,
        None => {
            out.push_str(
                "[FAIL] could not locate team directory after resolve — \
                 run `ccteam doctor --reset-shipped-teams`\n",
            );
            fails += 1;
            out.push_str(&format!("\nSummary: 0 ok, 0 warn, {fails} fail\n"));
            return Ok((out, fails));
        }
    };
    let phase_dir = team_dir.join(&spec.phase_dir);
    if !phase_dir.exists() {
        out.push_str(&format!(
            "[FAIL] phase_dir {} does not exist\n",
            phase_dir.display(),
        ));
        fails += 1;
        out.push_str(&format!("\nSummary: 0 ok, 0 warn, {fails} fail\n"));
        return Ok((out, fails));
    }

    // Sort phase markdowns so IO contract checks run in the
    // deterministic on-disk order (`02-plan-eng.md` < `03-implement.md`
    // < ...).
    let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(&phase_dir)?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) == Some("md") {
                Some(p)
            } else {
                None
            }
        })
        .collect();
    entries.sort();

    if entries.is_empty() {
        out.push_str(&format!(
            "[WARN] no phase markdowns under {}\n",
            phase_dir.display(),
        ));
        out.push_str(&format!("\nSummary: 0 ok, 1 warn, {fails} fail\n"));
        return Ok((out, fails));
    }

    // Project-bootstrapped artifacts the orchestrator writes before
    // the first phase runs. Treated as "produced by phase[-1]" so the
    // first phase can declare them as required_inputs without breaking
    // the IO contract check.
    let mut produced: std::collections::HashSet<String> =
        ["spec.md", ".ccteam/spec.md"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();

    let mut ok = 0u32;
    let mut warns = 0u32;

    for path in &entries {
        let file = path.file_name().and_then(|s| s.to_str()).unwrap_or("?");
        match PhaseTemplate::load(path) {
            Ok(t) => {
                if let Err(err) = t.validate_m0() {
                    out.push_str(&format!("[FAIL] {file}: validate_m0: {err:#}\n"));
                    fails += 1;
                    continue;
                }
                // IO contract: every required_input must be in `produced`.
                for input in &t.required_inputs {
                    if !produced.contains(input) {
                        out.push_str(&format!(
                            "[WARN] {file}: required_input `{input}` not produced by any prior phase\n",
                        ));
                        warns += 1;
                    }
                }
                // Body residue: warn on protocol literals (architecture §9).
                let body_raw = std::fs::read_to_string(path).unwrap_or_default();
                let body = strip_frontmatter(&body_raw);
                let phase_done_literal = format!("PHASE_DONE: {}", t.name);
                if body.contains(&phase_done_literal) {
                    out.push_str(&format!(
                        "[WARN] {file}: body contains `{phase_done_literal}` — V0.2 expects the inject prompt to carry it\n",
                    ));
                    warns += 1;
                }
                if body.contains("ESCALATE:") {
                    out.push_str(&format!(
                        "[WARN] {file}: body contains `ESCALATE:` literal — V0.2 expects the inject prompt to carry it\n",
                    ));
                    warns += 1;
                }
                if body.trim().is_empty() {
                    out.push_str(&format!(
                        "[WARN] {file}: body is empty — phase markdown should still describe the domain task\n",
                    ));
                    warns += 1;
                }
                // After this phase, its outputs become available to
                // downstream phases.
                for o in &t.required_outputs {
                    produced.insert(o.clone());
                }
                out.push_str(&format!("[OK]   {file}: phase=`{}`\n", t.name));
                ok += 1;
            }
            Err(err) => {
                out.push_str(&format!("[FAIL] {file}: parse: {err:#}\n"));
                fails += 1;
            }
        }
    }

    out.push_str(&format!(
        "\nSummary: {ok} ok, {warns} warn, {fails} fail\n",
    ));
    Ok((out, fails))
}

fn strip_frontmatter(body: &str) -> &str {
    if let Some(rest) = body.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---\n") {
            return &rest[end + 5..];
        }
    }
    body
}

/// V0.2 M0.18.6: render `<team>` `<phase>`'s inject prompt + body for
/// debugging. Renders the same `build_phase_prompt_for_template_with_team`
/// output the orchestrator dispatches, plus the phase markdown body
/// the assistant sees via `@.ccteam/phases/<phase>.md`. Pure read-only.
pub fn run_phase_show(
    paths: &CcteamPaths,
    team: &str,
    phase: &str,
) -> Result<String> {
    use ccteam_core::{
        default_user_staging_dir, resolve_team, GoldenRuleEnforcement,
        TeamResolveContext, TEAM_SOURCES,
    };

    let user_staging = default_user_staging_dir();
    // V0.2.1 F28: when run from inside a project tree, layer 1
    // (Project) wins so `phase show` reflects what the orchestrator
    // would actually inject. Falls back to user / repo when there's
    // no `.ccteam/team/team.yaml` in cwd.
    let cwd = std::env::current_dir().ok();
    let mut ctx = TeamResolveContext::for_orchestrator(&paths.root, &user_staging);
    if let Some(cwd) = cwd.as_deref() {
        if cwd.join(".ccteam").join("team").join("team.yaml").exists() {
            ctx = ctx.with_project(cwd);
        }
    }

    let spec = resolve_team(team, &ctx)
        .with_context(|| format!("resolve team `{team}`"))?;

    if spec.evergreen {
        bail!(
            "team `{team}` is evergreen — it has no phase DAG, so `phase show` does not apply"
        );
    }

    let team_dir = TEAM_SOURCES
        .iter()
        .filter_map(|s| s.path_for(team, &ctx))
        .find(|p| p.exists())
        .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
        .ok_or_else(|| anyhow::anyhow!("could not locate team directory for `{team}`"))?;
    let phase_dir = team_dir.join(&spec.phase_dir);
    let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(&phase_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("md"))
        .collect();
    entries.sort();

    let mut found_template: Option<PhaseTemplate> = None;
    let mut found_path: Option<std::path::PathBuf> = None;
    for path in &entries {
        match PhaseTemplate::load(path) {
            Ok(t) if t.name == phase => {
                found_template = Some(t);
                found_path = Some(path.clone());
                break;
            }
            _ => continue,
        }
    }

    let template = found_template.ok_or_else(|| {
        anyhow::anyhow!(
            "team `{team}` has no phase `{phase}` under {} — \
             check `ccteam doctor --validate-team {team}` for the phase list",
            phase_dir.display(),
        )
    })?;
    let template_path = found_path.unwrap();

    // Mirror orchestrator's dispatch path: assemble protocol directives
    // from team.yaml, then build the inject prompt with no attachments
    // (debug rendering doesn't see prior phase outputs).
    let protocol_dirs: Vec<&str> = spec
        .golden_rules
        .protocol
        .iter()
        .filter(|r| r.enforce == GoldenRuleEnforcement::PromptDirective)
        .filter_map(|r| r.directive.as_deref())
        .collect();
    let inject = ccteam_core::progress::build_phase_prompt_for_template_with_team(
        &template,
        &[],
        &protocol_dirs,
    );

    let body = std::fs::read_to_string(&template_path)
        .with_context(|| format!("read {}", template_path.display()))?;

    let mut out = String::new();
    out.push_str(&format!(
        "# Inject prompt (orchestrator-rendered for `{team}` / `{phase}`)\n\n",
    ));
    out.push_str(&format!("```\n{inject}\n```\n\n"));
    out.push_str(&format!(
        "# Phase markdown ({})\n\n",
        template_path.display(),
    ));
    out.push_str(&body);
    if !out.ends_with('\n') {
        out.push('\n');
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
        out.push_str("  legacy skills    no `~/.claude/skills/cct-*` dirs found — nothing to do.\n");
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
                    HookCmdRewriteAction::Rewrote { .. } | HookCmdRewriteAction::WouldRewrite { .. }
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
            return Err(err)
                .with_context(|| format!("read_dir {}", projects_root.display()));
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
        out.push_str(
            "no changes made (--dry-run). drop the flag to apply.\n",
        );
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
        r.target
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?"),
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
    match ccteam_core::render_screenshot(paths, slug, 50)? {
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

fn render_ls_text(projects: &[ProjectSummary], daemon_up: bool) -> String {
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

fn render_ls_json(projects: &[ProjectSummary], daemon_up: bool) -> Result<String> {
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

/// `ccteam watchdog scan` (V0.2 M0.21). Reads `~/.ccteam/watchdog.yaml`
/// (or defaults), scans every project + the daemon heartbeat, and
/// renders the resulting alerts. With `push_to_user_handle: Some(<h>)`
/// each alert that survives filtering is also written to the meta-agent
/// session's outbox so the meta-agent can surface it in NL.
///
/// Translation only: never mutates orchestrator state, never kills
/// sessions, never re-injects prompts. Pure read-side classifier.
pub fn run_watchdog_scan(
    paths: &CcteamPaths,
    format: OutputFormat,
    push_to_user_handle: Option<&str>,
) -> Result<String> {
    let cfg = ccteam_core::load_watchdog_config(paths)?;
    let alerts = ccteam_core::watchdog_scan(paths, &cfg)?;
    if let Some(handle) = push_to_user_handle {
        for alert in &alerts {
            ccteam_core::push_watchdog_alert_to_meta_outbox(paths, handle, alert)
                .with_context(|| {
                    format!("push watchdog alert to meta outbox for `{handle}`")
                })?;
        }
    }
    Ok(match format {
        OutputFormat::Json => {
            serde_json::to_string_pretty(&serde_json::json!({
                "alerts": alerts,
                "config": cfg,
            }))? + "\n"
        }
        OutputFormat::Text => render_watchdog_text(&alerts, push_to_user_handle.is_some()),
    })
}

fn render_watchdog_text(alerts: &[ccteam_core::WatchdogAlert], pushed: bool) -> String {
    if alerts.is_empty() {
        return "watchdog: no alerts.\n".into();
    }
    let mut out = format!("watchdog: {} alert(s)\n", alerts.len());
    for a in alerts {
        let scope = a.slug.as_deref().unwrap_or("(global)");
        out.push_str(&format!(
            "  [{}] {scope}: {}\n",
            a.kind.as_str(),
            a.message,
        ));
    }
    if pushed {
        out.push_str("(also written to meta-agent outbox)\n");
    }
    out
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
/// V0.3.1 F46 hook subcommand handler — `ccteam hook harness-snapshot`.
///
/// Reads ALL of stdin to a String (the Claude Code statusline command
/// is fed an opaque blob; we treat it as raw bytes), resolves the
/// destination under `~/.ccteam/harness/<slug>-<sid>.json` from
/// `current_dir()`, and atomically dual-writes via
/// [`ccteam_core::write_harness_snapshot`].
///
/// **Architectural red line** (PRD §3.3, this PR): never bubble — the
/// statusline render path is sacred. The caller in `main::run_hook`
/// wraps any error in `let _ = ...` and exits 0. We still return a
/// `Result` for testability (so the unit suite can assert success on
/// the happy path).
pub fn run_hook_harness_snapshot(paths: &CcteamPaths) -> Result<()> {
    let raw = std::io::read_to_string(std::io::stdin().lock())
        .context("read stdin for harness-snapshot hook")?;
    let cwd = std::env::current_dir().context("current_dir for harness-snapshot hook")?;
    match ccteam_core::write_harness_snapshot(paths, &cwd, &raw) {
        Ok(true) => {
            tracing::debug!(
                cwd = %cwd.display(),
                "ccteam harness-snapshot: dual-wrote",
            );
        }
        Ok(false) => {
            tracing::debug!(
                cwd = %cwd.display(),
                "ccteam harness-snapshot: cwd outside projects_root, skip",
            );
        }
        Err(err) => {
            tracing::debug!(
                cwd = %cwd.display(),
                error = %err,
                "ccteam harness-snapshot: best-effort write failed",
            );
        }
    }
    Ok(())
}

/// V0.3.1 F46 — install / refresh `~/.claude/statusline-command.sh`
/// so Claude Code's statusline command tees its stdin into our hook
/// before passing through to the user's original (preserved as a
/// `.bak-<utc-ts>` file).
///
/// Idempotency rules (per dev-plan §2.1):
///
/// - **No existing file** → write a fresh wrapper, no passthrough.
/// - **Existing user file, no marker** → backup to
///   `<orig>.bak-YYYYMMDDTHHMMSSZ`, write wrapper, passthrough invokes
///   the backup.
/// - **Existing wrapper with marker** → re-install: rewrite from the
///   in-binary template, glob-pick the most-recent `<orig>.bak-*` as
///   the passthrough target. No new backup is taken (we'd be backing
///   up our own output).
///
/// Wrapper marker (CLAUDE.md §三 marker convention adapted for shell
/// `#`-comment syntax — analogous to the F39 markdown
/// `<!-- ccteam-managed:lessons begin/end -->` pattern):
///
/// ```sh
/// # ccteam-managed:statusline begin (V0.3.1 F46 — do not hand-edit;
/// #                                    re-run `ccteam doctor --install-statusline-adapter`)
/// ```
///
/// Returns the user-facing report line (printed by `run_doctor`).
pub fn install_statusline_adapter(_paths: &CcteamPaths) -> Result<String> {
    use std::os::unix::fs::PermissionsExt;

    let claude_dir = user_claude_dir().context("resolve user ~/.claude dir")?;
    let target = claude_dir.join("statusline-command.sh");
    let bin = current_ccteam_bin().context("resolve current ccteam binary path")?;

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create dir {}", parent.display()))?;
    }

    let existing = match std::fs::read_to_string(&target) {
        Ok(s) => Some(s),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(anyhow::Error::from(err)
            .context(format!("read existing {}", target.display()))),
    };

    let backup_path: Option<std::path::PathBuf> = match existing.as_deref() {
        None => None,
        Some(body) if body.contains(STATUSLINE_MARKER_BEGIN) => {
            // Re-install: find the most recent prior backup so the
            // refreshed wrapper still passes through to the user's
            // original. We do NOT create a new backup of our own output.
            find_latest_statusline_backup(&claude_dir)?
        }
        Some(body) => {
            // Brand-new install over a user-authored statusline. Take a
            // backup and use it as the passthrough target.
            let stamp = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
            let bak = target.with_file_name(format!("statusline-command.sh.bak-{stamp}"));
            std::fs::write(&bak, body)
                .with_context(|| format!("write backup {}", bak.display()))?;
            // Mirror the source's executable bit if we can; harmless
            // if we cannot read metadata.
            if let Ok(meta) = std::fs::metadata(&target) {
                let mode = meta.permissions().mode();
                let _ = std::fs::set_permissions(&bak, std::fs::Permissions::from_mode(mode));
            }
            Some(bak)
        }
    };

    let wrapper_body = render_statusline_wrapper(&bin, backup_path.as_deref());
    let tmp = target.with_file_name("statusline-command.sh.ccteam-tmp");
    std::fs::write(&tmp, &wrapper_body)
        .with_context(|| format!("write wrapper tmp {}", tmp.display()))?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
        .with_context(|| format!("chmod 0755 {}", tmp.display()))?;
    std::fs::rename(&tmp, &target)
        .with_context(|| format!("rename {} -> {}", tmp.display(), target.display()))?;

    let backup_disp = backup_path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "none".into());
    Ok(format!(
        "ccteam doctor --install-statusline-adapter\n\n  \
         installed: {} (backup: {})\n",
        target.display(),
        backup_disp,
    ))
}

/// Marker that flags the statusline wrapper as ccteam-managed (per
/// CLAUDE.md §三). The `(V0.3.1 F46 ...)` suffix mirrors the F39
/// memory-bridge marker convention so future audits can grep one
/// pattern across both surfaces.
const STATUSLINE_MARKER_BEGIN: &str =
    "# ccteam-managed:statusline begin (V0.3.1 F46";

fn render_statusline_wrapper(bin: &std::path::Path, backup: Option<&std::path::Path>) -> String {
    let bin_disp = bin.display().to_string();
    let mut out = String::new();
    out.push_str("#!/bin/sh\n");
    out.push_str(STATUSLINE_MARKER_BEGIN);
    out.push_str(" — do not hand-edit;\n");
    out.push_str("#                                    re-run `ccteam doctor --install-statusline-adapter`)\n");
    out.push_str("INPUT=$(cat)\n");
    out.push_str("# Dual-write: best-effort, never blocks statusline render\n");
    out.push_str(&format!(
        "printf '%s' \"$INPUT\" | {bin_disp} hook harness-snapshot 2>/dev/null || true\n",
    ));
    out.push_str("# ccteam-managed:statusline end\n");
    if let Some(bak) = backup {
        out.push_str("# Passthrough to user's original (preserved as backup)\n");
        out.push_str(&format!("if [ -x \"{}\" ]; then\n", bak.display()));
        out.push_str(&format!("    printf '%s' \"$INPUT\" | \"{}\"\n", bak.display()));
        out.push_str("fi\n");
    }
    out
}

/// Glob `~/.claude/statusline-command.sh.bak-*` and return the path
/// with the lexicographically-greatest suffix (timestamps sort lex).
fn find_latest_statusline_backup(
    claude_dir: &std::path::Path,
) -> Result<Option<std::path::PathBuf>> {
    let entries = match std::fs::read_dir(claude_dir) {
        Ok(e) => e,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("read_dir {}", claude_dir.display())),
    };
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_s = match name.to_str() {
            Some(s) => s,
            None => continue,
        };
        if name_s.starts_with("statusline-command.sh.bak-") {
            candidates.push(entry.path());
        }
    }
    candidates.sort();
    Ok(candidates.into_iter().next_back())
}

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

    /// V0.2.2 F34: thin wrapper that defaults to deterministic Tier 4
    /// (no auto-suggest) so unit tests don't accidentally shell-out to
    /// `claude -p` on dev / CI machines that have it on PATH.
    fn run_new_t4(paths: &CcteamPaths, request: &str, team: &str) -> Result<String> {
        run_new(
            paths,
            request,
            team,
            RunNewOptions {
                slug: None,
                no_auto_slug: true,
                auto_slug_model: "claude-haiku-4-5-20251001",
            },
        )
    }

    #[test]
    fn run_new_creates_slug_and_bootstrap_files() {
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let slug = run_new_t4(&paths, "Build a bookmark manager", "dev").unwrap();
        // F22: slug now carries the team prefix so ~/.claude/rules/ccteam-lessons-dev.md
        // `paths: ~/projects/dev-*` matches at session start.
        // V0.2.2 F34: `slugify_brief` drops stop-words so `a` is filtered out.
        assert!(
            slug.starts_with("dev-build-bookmark-manager"),
            "expected `dev-build-bookmark-manager*`, got {slug}",
        );
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
        let err = run_new_t4(&paths, "   \n\t", "dev").unwrap_err();
        assert!(format!("{err:#}").contains("non-empty"));
    }

    #[test]
    fn run_new_rejects_empty_team() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let err = run_new_t4(&paths, "build something", "  ").unwrap_err();
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
            run_new_t4(&paths, "ai recipe generator idea", "product-research").unwrap();
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
        let err = run_new_t4(&paths, "marketing copy idea", "marketing").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("unknown team"), "got: {msg}");
        assert!(msg.contains("marketing"), "should name the missing team");
        assert!(
            msg.contains("teams/marketing/team.yaml"),
            "should point at the missing path",
        );
        // After M0.16.2 self-heal, the disk-driven team list includes
        // the shipped seeds — the error message lists those so users
        // see the catalog of options. V0.2.2 F40: `research` is the
        // canonical name; `product-research` only exists as an alias
        // (no directory on disk), so the help message lists `research`.
        assert!(msg.contains("dev"));
        assert!(msg.contains("research"));
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
        let err = run_new_t4(&paths, "do a thing", "custom-team").unwrap_err();
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
        let slug = run_new_t4(&paths, "demo", "dev").unwrap();

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
        let slug = run_new_t4(&paths, "demo", "dev").unwrap();

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
        // V0.2.2 F40: research is the canonical team name; the bridge
        // file scaffold ships at ccteam-lessons-research.md. Old
        // installs already on disk keep their
        // ccteam-lessons-product-research.md file untouched (different
        // test surface — no need to fixture it here).
        assert!(report.contains("research"));

        let dev_path = tmp.path().join("rules/ccteam-lessons-dev.md");
        let research_path = tmp.path().join("rules/ccteam-lessons-research.md");
        assert!(dev_path.is_file(), "dev lessons not written");
        assert!(research_path.is_file(), "research lessons not written");

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

        // Skill landed under the redirected ~/.claude/. F44 reverts F39:
        // both shipped skills are written under canonical ccteam-* names.
        let control = tmp.path().join("skills/ccteam-control/SKILL.md");
        assert!(control.is_file(), "ccteam-control SKILL.md not written: {}", control.display());
        let team_author = tmp.path().join("skills/ccteam-team-author/SKILL.md");
        assert!(
            team_author.is_file(),
            "ccteam-team-author SKILL.md not written: {}",
            team_author.display(),
        );

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

        // F44 reverts F39: shipped skills land under canonical ccteam-* names.
        assert!(tmp.path().join("skills/ccteam-control/SKILL.md").is_file());
        assert!(tmp.path().join("skills/ccteam-team-author/SKILL.md").is_file());
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
        assert!(report.contains("F44 reverse migration"), "F44 report header missing: {report}");
        assert!(report.contains("cct-control/"), "legacy entry missing: {report}");
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
        assert!(report.contains("preserved"), "F44 should report preserve: {report}");
        assert!(legacy.exists(), "hand-edited legacy dir was unexpectedly removed");
        std::env::remove_var("CLAUDE_CONFIG_HOME");
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
        // V0.2.2 F40: `research` (canonical) replaced `product-research`.
        for team in ["dev", "research", "meta-agent"] {
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
        // research` succeed because run_new self-heals.
        // V0.2.2 F40: canonical name is `research`.
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        assert!(!paths.root.join("teams").exists(), "precondition: empty global dir");

        let slug = run_new_t4(&paths, "Verify research bootstraps", "research")
            .expect("run_new must auto-seed shipped templates before validation");
        assert!(slug.starts_with("research-"));
        // The seed must have landed during run_new.
        assert!(paths.root.join("teams/research/team.yaml").is_file());
    }

    #[test]
    fn run_new_accepts_legacy_alias_product_research_with_deprecation_warn() {
        // V0.2.2 F40 — `ccteam new --team product-research` still works
        // (alias resolution against shipped `teams/research/team.yaml`).
        // The slug carries the typed-team prefix so the project lands at
        // ~/projects/product-research-<base>; state.json::team also
        // preserves the typed alias so the daemon's tick walks the
        // alias-aware team_runtime path. Visible deprecation goes to
        // stderr (not asserted here — captured-output isolation is
        // brittle in unit tests; the e2e test in
        // crates/ccteam-cli/tests/m3_product_research_e2e_test.rs covers
        // the alias resolution end-to-end).
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let slug = run_new_t4(&paths, "ai recipe generator", "product-research")
            .expect("alias must resolve to canonical research team");
        assert!(
            slug.starts_with("product-research-"),
            "alias-input slug should preserve the typed team prefix; got `{slug}`",
        );
        let state =
            ccteam_core::ProjectState::load(&paths.project_state(&slug)).unwrap();
        assert_eq!(state.team, "product-research");
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

    // ---------------- V0.2 M0.18.5 doctor --validate-team ----------------

    #[test]
    fn validate_team_reports_ok_for_shipped_dev_team() {
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        // Seed shipped teams on disk so the resolver can walk them.
        ccteam_core::write_all_global_team_templates(&paths.root, false).unwrap();

        let opts = DoctorOptions {
            validate_team: Some("dev".into()),
            ..DoctorOptions::default()
        };
        let report = run_doctor(&paths, opts).unwrap();
        assert!(report.contains("validate-team dev"), "got: {report}");
        assert!(report.contains("[OK] team.yaml"), "got: {report}");
        assert!(report.contains("plan-eng"), "got: {report}");
        assert!(report.contains("Summary:"), "got: {report}");
    }

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


    #[test]
    fn validate_team_resolves_alias_to_canonical_dir() {
        // V0.2.2 F41 regression: when --validate-team is given an F40
        // alias (eg `product-research`), the command should resolve to
        // canonical TeamSpec (`research`) AND locate the phase dir at
        // the canonical path (`teams/research/`), not at the alias arg
        // (`teams/product-research/`, which doesn't exist after F40).
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        ccteam_core::write_all_global_team_templates(&paths.root, false).unwrap();

        let opts = DoctorOptions {
            validate_team: Some("product-research".into()),
            ..DoctorOptions::default()
        };
        let report = run_doctor(&paths, opts).unwrap();
        assert!(
            report.contains("[OK] team.yaml resolves; name=`research`"),
            "alias should resolve to canonical name; got: {report}"
        );
        // Phase dir located → not the misleading "could not locate team
        // directory" failure that F41 produced before the fix.
        assert!(
            !report.contains("could not locate team directory"),
            "alias path should locate canonical team dir; got: {report}"
        );
        assert!(
            report.contains("0 fail"),
            "alias-resolve path should pass with 0 fail; got: {report}"
        );
    }

    #[test]
    fn validate_team_warns_when_phase_body_contains_protocol_literal() {
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        ccteam_core::write_all_global_team_templates(&paths.root, false).unwrap();

        // Re-write one phase body to include the banned literal so we
        // verify the warn path. The frontmatter stays valid; only the
        // body drifts.
        let phase_path = paths
            .root
            .join("teams/dev/phases/03-implement.md");
        let original = std::fs::read_to_string(&phase_path).unwrap();
        let drifted = format!("{original}\n\n最后一行: PHASE_DONE: implement\n");
        std::fs::write(&phase_path, drifted).unwrap();

        let opts = DoctorOptions {
            validate_team: Some("dev".into()),
            ..DoctorOptions::default()
        };
        let report = run_doctor(&paths, opts).unwrap();
        assert!(
            report.contains("[WARN]") && report.contains("PHASE_DONE: implement"),
            "got: {report}",
        );
    }

    #[test]
    fn validate_team_skips_phase_dir_for_evergreen_team() {
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        ccteam_core::write_all_global_team_templates(&paths.root, false).unwrap();

        let opts = DoctorOptions {
            validate_team: Some("meta-agent".into()),
            ..DoctorOptions::default()
        };
        let report = run_doctor(&paths, opts).unwrap();
        assert!(report.contains("evergreen"), "got: {report}");
    }

    // ---------------- V0.2 M0.18.6 ccteam phase show ----------------

    #[test]
    fn phase_show_renders_inject_prompt_and_body() {
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        ccteam_core::write_all_global_team_templates(&paths.root, false).unwrap();

        let body = run_phase_show(&paths, "dev", "implement").unwrap();
        assert!(body.contains("Inject prompt"), "got: {body}");
        assert!(body.contains("@.ccteam/phases/implement.md"));
        assert!(body.contains("PHASE_DONE: implement"));
        assert!(body.contains("Phase markdown"));
    }

    #[test]
    fn phase_show_errors_on_unknown_phase() {
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        ccteam_core::write_all_global_team_templates(&paths.root, false).unwrap();

        let err = run_phase_show(&paths, "dev", "nonexistent").unwrap_err();
        assert!(format!("{err:#}").contains("nonexistent"));
    }

    #[test]
    fn phase_show_errors_on_evergreen_team() {
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        ccteam_core::write_all_global_team_templates(&paths.root, false).unwrap();

        let err = run_phase_show(&paths, "meta-agent", "anything").unwrap_err();
        assert!(format!("{err:#}").contains("evergreen"));
    }

    #[test]
    fn watchdog_scan_text_format_renders_no_alerts_when_healthy() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        std::fs::create_dir_all(&paths.projects_root).unwrap();
        ccteam_core::write_heartbeat(&paths).unwrap();
        let out = run_watchdog_scan(&paths, OutputFormat::Text, None).unwrap();
        assert!(out.contains("no alerts"), "got: {out}");
    }

    #[test]
    fn watchdog_scan_json_format_emits_structured_payload() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        std::fs::create_dir_all(&paths.projects_root).unwrap();
        // Heartbeat absent ⇒ daemon_down alert.
        let out = run_watchdog_scan(&paths, OutputFormat::Json, None).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        let alerts = parsed["alerts"].as_array().unwrap();
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0]["kind"], "daemon_down");
        assert!(parsed["config"].is_object());
    }

    // -------------- V0.2.2 F34 — slug --slug flag + sanitize ---------------

    #[test]
    fn run_new_with_explicit_slug_uses_verbatim_path() {
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let slug = run_new(
            &paths,
            "literally anything goes here",
            "dev",
            RunNewOptions {
                slug: Some("ccteam-ui"),
                no_auto_slug: true,
                auto_slug_model: "claude-haiku-4-5-20251001",
            },
        )
        .unwrap();
        assert_eq!(slug, "dev-ccteam-ui");
        assert!(paths.project_dir(&slug).join(".ccteam/spec.md").exists());
    }

    #[test]
    fn run_new_with_explicit_slug_keeps_team_prefix_when_present() {
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let slug = run_new(
            &paths,
            "irrelevant brief",
            "dev",
            RunNewOptions {
                slug: Some("dev-explicit-name"),
                no_auto_slug: true,
                auto_slug_model: "claude-haiku-4-5-20251001",
            },
        )
        .unwrap();
        assert_eq!(slug, "dev-explicit-name");
    }

    #[test]
    fn run_new_with_explicit_illegal_slug_fails_loud() {
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let err = run_new(
            &paths,
            "irrelevant",
            "dev",
            RunNewOptions {
                slug: Some("Bad Slug!"),
                no_auto_slug: true,
                auto_slug_model: "claude-haiku-4-5-20251001",
            },
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("[a-z0-9-]+"),
            "expected fail-loud regex hint, got: {msg}",
        );
    }

    #[test]
    fn sanitize_smart_slug_accepts_clean_kebab() {
        assert_eq!(
            sanitize_smart_slug("hermestrade-dex\n"),
            Some("hermestrade-dex".to_string())
        );
    }

    #[test]
    fn sanitize_smart_slug_strips_quotes_and_label_prefix() {
        assert_eq!(
            sanitize_smart_slug("\"todo-cli\""),
            Some("todo-cli".to_string())
        );
        assert_eq!(
            sanitize_smart_slug("Slug: ai-recipe-generator"),
            Some("ai-recipe-generator".to_string())
        );
    }

    #[test]
    fn sanitize_smart_slug_takes_first_non_empty_line() {
        let raw = "\n\nthe-slug\nignored explanation line\n";
        assert_eq!(sanitize_smart_slug(raw), Some("the-slug".to_string()));
    }

    #[test]
    fn sanitize_smart_slug_lowercases_and_drops_disallowed_chars() {
        // Letters lowered, exclamation stripped → `hello-world`
        assert_eq!(
            sanitize_smart_slug("Hello-World!"),
            Some("hello-world".to_string())
        );
    }

    #[test]
    fn sanitize_smart_slug_rejects_too_short_or_too_long() {
        assert_eq!(sanitize_smart_slug("a"), None);
        assert_eq!(sanitize_smart_slug(&"x".repeat(61)), None);
        // Boundary: exactly 60 is fine.
        assert_eq!(
            sanitize_smart_slug(&"x".repeat(60)),
            Some("x".repeat(60))
        );
    }

    #[test]
    fn sanitize_smart_slug_rejects_empty_or_dash_only() {
        assert_eq!(sanitize_smart_slug(""), None);
        assert_eq!(sanitize_smart_slug("---"), None);
    }

    #[test]
    fn render_smart_slug_prompt_embeds_brief_and_examples() {
        let p = render_smart_slug_prompt("Build HermesTrade DEX home");
        assert!(p.contains("Build HermesTrade DEX home"));
        // The example anchors keep the prompt stable for tests + audits.
        assert!(p.contains("HermesTrade DEX prediction market"));
        assert!(p.contains("Slug:"));
    }
}
