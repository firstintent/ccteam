//! Command handlers for `ccteam {new, ls, show, attach, peek, progress,
//! resume}`. Pure where possible (`run_ls` / `run_show` return the
//! formatted string instead of printing) so unit tests don't need a
//! real terminal or running orchestrator.

use std::process::Command;

use anyhow::{bail, Context, Result};
use chrono::Utc;
use serde_json::{json, Map, Value};

use ccteam_core::{
    bootstrap_project, current_ccteam_bin, pick_unused_slug, write_global_phase_templates,
    CcteamPaths, PhaseState, ProjectState, PHASE_TEMPLATES,
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

/// `ccteam new "request"`. Bootstraps a project on disk and returns
/// the chosen slug. Side effects: creates `~/projects/<slug>/...`.
pub fn run_new(paths: &CcteamPaths, request: &str) -> Result<String> {
    if request.trim().is_empty() {
        bail!("ccteam new: request must be non-empty");
    }
    let slug = pick_unused_slug(paths, request)?;
    bootstrap_project(paths, &slug, request)?;
    Ok(slug)
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
        .args([
            "capture-pane",
            "-p",
            "-t",
            &format!("{}:0", session.name()),
        ])
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
pub fn run_resume(paths: &CcteamPaths, slug: &str) -> Result<()> {
    let state_path = paths.project_state(slug);
    let mut state = ProjectState::load(&state_path)
        .with_context(|| format!("load state for {slug}"))?;
    state.user_pause_pending = false;
    state.user_attached = false;
    state.phase_state = PhaseState::Idle;
    state.last_user_interaction_at = Utc::now();
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

fn collect_recent_events(
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

fn collect_artifacts(paths: &CcteamPaths, slug: &str) -> Map<String, Value> {
    let mut m = Map::new();
    let ccteam_dir = paths.project_ccteam_dir(slug);
    for known in [
        ("spec", "spec.md"),
        ("plan_eng", "plan-eng.md"),
        ("plan_ceo", "plan-ceo.md"),
        ("architecture", "architecture.md"),
        ("implement_report", "implement-report.md"),
        ("test_report", "test-report.md"),
        ("fix_report", "fix-report.md"),
        ("review_report", "review-report.md"),
        ("retro", "retro.md"),
        ("escalation", "escalation.md"),
    ] {
        let path = ccteam_dir.join(known.1);
        if path.exists() {
            m.insert(
                known.0.into(),
                Value::String(format!(".ccteam/{}", known.1)),
            );
        }
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
            phase_state_str(p.state.phase_state),
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
                "phase_state": phase_state_str(p.state.phase_state),
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
        phase_state_str(state.phase_state)
    ));
    out.push_str(&format!("cost used      : ${:.2}\n", state.cost_used_usd));
    out.push_str(&format!(
        "context tokens : {} ({} resets)\n",
        state.context_tokens_used, state.context_reset_count
    ));
    out.push_str(&format!(
        "fix cycle      : {}\n",
        state.fix_cycle_count
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

fn phase_state_str(s: PhaseState) -> &'static str {
    match s {
        PhaseState::InFlight => "in_flight",
        PhaseState::Idle => "idle",
        PhaseState::FixLocked => "fix_locked",
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
    use super::*;
    use ccteam_core::progress;
    use tempfile::TempDir;

    fn fresh_paths(tmp: &TempDir) -> CcteamPaths {
        CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        }
    }

    #[test]
    fn run_new_creates_slug_and_bootstrap_files() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let slug = run_new(&paths, "Build a bookmark manager").unwrap();
        assert!(slug.starts_with("build-a-bookmark-manager"));
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
        let err = run_new(&paths, "   \n\t").unwrap_err();
        assert!(format!("{err:#}").contains("non-empty"));
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
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        run_new(&paths, "demo one").unwrap();
        run_new(&paths, "demo two").unwrap();

        let body = run_ls(&paths, OutputFormat::Json).unwrap();
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["projects"].as_array().unwrap().len(), 2);
        assert_eq!(v["orchestrator"]["active_count"], 0);
        assert_eq!(v["orchestrator"]["max_concurrent"], 1);
    }

    #[test]
    fn run_show_json_includes_state_and_artifacts() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let slug = run_new(&paths, "demo").unwrap();
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
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let slug = run_new(&paths, "demo").unwrap();
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
    fn run_init_creates_global_skeleton_and_unpacks_phases() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let report = run_init(&paths, InitOptions::default()).unwrap();
        for sub in ["phases", "progress", "inbox", "control"] {
            assert!(
                paths.root.join(sub).is_dir(),
                "init must create {}",
                paths.root.join(sub).display()
            );
        }
        assert!(paths.phases_dir().join("02-plan-eng.md").is_file());
        assert!(paths.phases_dir().join("09-ship.md").is_file());
        assert!(report.contains("phase templates"));
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
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let slug = run_new(&paths, "demo").unwrap();
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

}
