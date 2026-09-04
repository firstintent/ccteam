//! `ccteam flow` — author (`new`), drive (`run`) and judge (`eval`) one
//! dynamic workflow. `new` and `eval` are both sugar over `run`: one writes a
//! scaffold and prints the script surface, the other resolves which script
//! evaluates a finished run and then *is* an ordinary run.
//!
//! The runner itself lives in `ccteam-flow` and knows nothing about `$HOME`,
//! terminals or credentials. This module supplies the three things it cannot
//! resolve for itself, and nothing else:
//!
//! 1. **Who we are** — the machine-wide enrollment credential, minted by the
//!    very same call `ccteam config mcp` makes ([`crate::mcp_serve::enroll_bearer`]).
//!    A workflow is a hand-started client like any other; it does not get a
//!    credential family of its own.
//! 2. **Where we work** — `--project`, else the slug written in the cwd's
//!    `.ccteam/state.json`. A user-scoped credential names no project, so this
//!    has to be explicit; ccteam never infers a workspace from a path.
//! 3. **Where the run is remembered** — a directory under
//!    `<ccteam home>/runs/`, so `--resume` has somewhere to resume from.
//!
//! Output is split on purpose: progress goes to stderr one line at a time, the
//! final report goes to stdout as one line of compact JSON. `ccteam flow run
//! x.js > report.json` therefore works while the operator still watches the
//! run happen.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use ccteam_core::CcteamPaths;
use ccteam_flow::{
    run_workflow, FlowClient, McpEndpoint, McpFlowClient, ProgressCallback, ProgressEvent,
    RunConfig, RunReport, ScriptSource,
};
use serde_json::{json, Value};

/// `<ccteam home>/runs` — one directory per run (journal, script, results).
/// Registered in `ccteam_core::canonical_home_dirs()` so `ccteam doctor`'s
/// home-drift check does not report it as an orchestrator-era leftover.
const RUNS_DIR: &str = "runs";

/// `<ccteam home>/flows` — the machine-wide rung of evaluator lookup. Also
/// registered in `ccteam_core::canonical_home_dirs()`, same reason as `runs`.
const FLOWS_DIR: &str = "flows";

/// The evaluator `ccteam flow eval` resolves to when it is not given one, on
/// both rungs. Leading underscore so it sorts above a project's real flows and
/// reads as machinery rather than as one more workflow to run.
const EVAL_SCRIPT_FILE: &str = "_eval.flow.js";

/// A project's committed flow scripts: `.agents/flows/`, the sibling of
/// `.agents/skills`. Deliberately NOT `.ccteam/` — ccteam adds that directory
/// to the project's `.gitignore`, so a script there falls silently out of
/// version control (which is exactly why per-checkout *hooks* do live there).
fn project_flows_dir(project_dir: &Path) -> PathBuf {
    project_dir.join(".agents").join("flows")
}

/// A directory is a workflow run exactly when it holds a journal. One
/// predicate for both verbs, so `flow eval <dir>` and `flow run --resume <dir>`
/// can never disagree about what a run directory is.
fn is_run_dir(dir: &Path) -> bool {
    dir.join("journal.jsonl").is_file()
}

/// Everything `ccteam flow run` was asked for, already parsed by clap.
pub struct FlowRunRequest {
    pub script: PathBuf,
    pub project: Option<String>,
    pub args: Option<String>,
    pub parallel: Option<usize>,
    pub max_agents: Option<usize>,
    pub max_cost: Option<f64>,
    pub budget: Option<f64>,
    pub run_dir: Option<PathBuf>,
    pub resume: Option<PathBuf>,
    pub watchdog: Option<u64>,
    /// Managed session to attribute hires to; `None` falls back to
    /// `$CCTEAM_CHAT_SID` (present inside every ccteam-managed session).
    pub parent: Option<String>,
}

/// `--parent` beats the ambient `CCTEAM_CHAT_SID`; a blank env value counts
/// as absent. Pure so the precedence is unit-testable without touching
/// process env (the workspace forbids env writes in lib tests).
fn resolve_parent(flag: Option<String>, env_sid: Option<String>) -> Option<String> {
    flag.or(env_sid).filter(|sid| !sid.trim().is_empty())
}

/// Run one workflow to completion. Returns `Err` exactly when the report is
/// not `ok` — a script that threw, or a brake that ended the run early — so
/// the exit code is usable in a pipeline.
pub fn run(req: FlowRunRequest) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    if !req.script.is_file() {
        return Err(anyhow!(
            "no workflow script at {} (`ccteam flow run <file.js>`)",
            req.script.display()
        ));
    }
    let project = match req.project {
        Some(slug) => slug,
        None => project_from_cwd(&paths)?,
    };
    let args = req
        .args
        .as_deref()
        .map(|raw| serde_json::from_str::<Value>(raw).context("--args must be valid JSON"))
        .transpose()?;
    let (run_dir, resuming) = resolve_run_dir(&paths, &req.script, req.run_dir, req.resume)?;
    // A flow launched from inside a managed session hangs its leaves under that
    // session in the delegation tree — the runner itself is only an enrolled
    // client (the common case IS a managed session triggering runs). Resolved
    // here, not at the `cfg` assignment below, because the ledger bridge stamps
    // the same attribution onto the run's envelope rows.
    let parent_sid = resolve_parent(req.parent.clone(), std::env::var("CCTEAM_CHAT_SID").ok());
    // Second progress sink: the run-level envelope goes to the project ledger so
    // the daemon and the web UI can see a run at all. Purely additive — it
    // cannot fail the run, and dropping it flushes before the process exits.
    let ledger = crate::flow_bridge::LedgerBridge::start(
        &paths,
        &project,
        &run_dir,
        &req.script,
        parent_sid.clone(),
    );

    let endpoint = McpEndpoint {
        url: ccteam_harness::execution::mcp_config::resolve_mcp_http_url(&paths.root.join("run")),
        bearer: crate::mcp_serve::enroll_bearer(&paths)?,
        project,
    };
    let client: Arc<dyn FlowClient> =
        Arc::new(McpFlowClient::new(endpoint).map_err(|err| anyhow!("{err}"))?);

    let mut cfg = RunConfig::new(&run_dir, client);
    cfg.args = args;
    cfg.resume = resuming;
    cfg.watchdog = req.watchdog.map(Duration::from_secs);
    cfg.parent_sid = parent_sid;
    if let Some(parallel) = req.parallel {
        cfg.scheduler.max_parallel = parallel.max(1);
    }
    if let Some(max_agents) = req.max_agents {
        cfg.brakes.max_agents = max_agents;
    }
    cfg.brakes.max_cost_usd = req.max_cost;
    cfg.brakes.budget_total = req.budget;
    cfg.progress = Some(crate::flow_bridge::tee(stderr_progress(), ledger.sink()));

    // A current-thread runtime is enough: every wait in a run is I/O (an HTTP
    // long poll), and the JS engine is single-threaded by construction.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("tokio runtime")?;
    let report = runtime
        .block_on(run_workflow(ScriptSource::path(&req.script), cfg))
        .map_err(|err| anyhow!("{err}"))?;

    println!("{}", report_json(&report));
    if report.ok() {
        return Ok(());
    }
    Err(anyhow!(
        "{}",
        report
            .script_error
            .clone()
            .or_else(|| report.brake.clone().map(|why| format!("brake: {why}")))
            .unwrap_or_else(|| "workflow did not complete".to_string())
    ))
}

/// Everything `ccteam flow new` was asked for.
pub struct FlowNewRequest {
    pub name: String,
    /// Explicit destination; `None` picks the project's `.agents/flows/`.
    pub dir: Option<PathBuf>,
}

/// Scaffold one flow script, then print the script surface to stdout.
///
/// The printing is the point, not a courtesy. Claude Code can afford to bake
/// its authoring manual into a tool's JSON-schema `description` — it rides
/// into every session for free. ccteam cannot: injecting anything into a
/// session is the standing no-prompt-injection red line, and MCP tool schemas
/// tax every session's context whether or not that session writes flows. So
/// the manual is *earned* instead — an agent that runs `flow new` asked for
/// it, and gets it on stdout where its shell already looks.
pub fn new_script(req: FlowNewRequest) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    let slug = ccteam_core::slugify(&req.name);
    let dir = match req.dir {
        Some(dir) => dir,
        None => default_new_dir(&paths)?,
    };
    let file = write_scaffold(&dir, &slug)?;

    println!("created {}", file.display());
    println!();
    print!("{CHEAT_SHEET}");
    println!();
    println!("edit it, then: ccteam flow run {}", file.display());
    Ok(())
}

/// Materialize `<dir>/<slug>.flow.js`, refusing to clobber. The whole value of
/// a scaffold is that it is safe to reach for, and a scaffold that can eat a
/// written flow is not — so an existing file is an error, never an overwrite.
fn write_scaffold(dir: &Path, slug: &str) -> Result<PathBuf> {
    use std::io::Write as _;
    std::fs::create_dir_all(dir)
        .with_context(|| format!("create flow directory {}", dir.display()))?;
    let file = dir.join(format!("{slug}.flow.js"));
    // `create_new` makes the existence check and the create one atomic O_EXCL
    // open — no windowed check-then-write to race, and a dangling symlink at
    // the target is refused rather than followed (checker s523 R1).
    let mut handle = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&file)
    {
        Ok(handle) => handle,
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(anyhow!(
                "{} already exists — pick another name, or pass `--dir`",
                file.display()
            ));
        }
        Err(err) => return Err(err).with_context(|| format!("write {}", file.display())),
    };
    handle
        .write_all(script_template(slug).as_bytes())
        .with_context(|| format!("write {}", file.display()))?;
    Ok(file)
}

/// `--dir` beats everything. Otherwise a flow belongs with the project that
/// runs it, and a cwd outside any project just gets the cwd — ccteam never
/// invents a workspace for a path (same rule [`project_from_cwd`] follows,
/// minus the hard failure: scaffolding a file needs no daemon identity).
fn default_new_dir(paths: &CcteamPaths) -> Result<PathBuf> {
    let cwd = std::env::current_dir().context("resolve current directory")?;
    match ccteam_core::session_context_from_cwd(&cwd, paths) {
        Ok(context) => Ok(project_flows_dir(&context.project_dir)),
        Err(_) => Ok(cwd),
    }
}

/// The scaffold is CODE, not content: a `meta` block and comments. Even a
/// placeholder `agent('TODO: …')` line is a prompt string an unedited run
/// would send to a model, so the template ships NO agent call at all — the
/// first task's wording is the author's, and prompt-shaped content does not
/// ship in this repo (checker s523 R1).
fn script_template(slug: &str) -> String {
    format!(
        r#"export const meta = {{ name: '{slug}', description: 'TODO: one line' }}

// Replace with your orchestration. Available globals: agent(task, opts?),
// parallel([...thunks]), pipeline(items, ...stages), phase(title), log(msg),
// args, budget, usage(). Full reference: docs/hook-dynamic-workflows.md
return null
"#
    )
}

/// The script surface, verbatim from `docs/hook-dynamic-workflows.md` — short
/// enough to read in a terminal, complete enough to write a correct flow from
/// without opening anything else.
const CHEAT_SHEET: &str = "\
Script globals
  agent(task, opts?)      the worker's final text; the validated object when
                          opts.schema matched; null on ANY worker-side failure
                          (vendor error, guardrail or policy refusal)
  parallel([...thunks])   barrier; a failed slot is null, the call never rejects
  pipeline(items, ...st)  no barrier between stages — item A can be in stage 3
                          while B is in stage 1; a stage throw nulls that item
                          and skips its remaining stages
  phase(title) / log(m)   progress structure and narration
  args                    the --args value, verbatim
  budget                  {total, spent(), remaining()} in USD, summed live
  usage()                 per-harness quota map, as status{detail:\"usage\"}

agent opts
  vendor (claude default) · model · effort · role · sid (follow up on an
  existing session) · keep (don't stop the worker) · label (also the ledger
  title) · phase · permission_mode · schema + retry:{max,prompt}.
  An unknown option is a hard error, not a silent ignore.

Brakes vs failures
  A worker failing resolves that call to null. A brake (max_agents, max-cost,
  wall clock, budget) refuses NEW admissions instead: a direct `await agent()`
  throws an error naming the brake, parallel/pipeline slots mask to null,
  RunReport.brake names it either way, and in-flight workers always finish.

Determinism
  No filesystem, network or process access; Date.now(), Math.random() and
  argless new Date() throw — pass time and randomness in through --args.
  That discipline is what makes --resume exact.
";

/// Everything `ccteam flow eval` was asked for.
pub struct FlowEvalRequest {
    /// A run directory, or the bare id of one under `<ccteam home>/runs/`.
    pub run: String,
    pub script: Option<PathBuf>,
    pub project: Option<String>,
    pub max_cost: Option<f64>,
    pub parent: Option<String>,
}

/// Evaluate a finished run with a flow of your own.
///
/// This is sugar, and stays sugar: it resolves *which* script judges the run,
/// then hands the job to [`run`] unchanged. An evaluation IS a flow run — it
/// gets its own run directory, journal, resume and report for free, and there
/// is exactly one runner to keep honest. The engine still judges nothing
/// itself; the verdict comes from agents inside a script the user wrote.
pub fn eval(req: FlowEvalRequest) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    let target = resolve_eval_target(&paths, &req.run)?;
    let script = match req.script {
        Some(path) => {
            if !path.is_file() {
                return Err(anyhow!("no evaluator script at {}", path.display()));
            }
            path
        }
        None => resolve_evaluator(&paths, project_dir_for(&paths, req.project.as_deref()))?,
    };
    run(FlowRunRequest {
        script,
        project: req.project,
        args: Some(json!({ "run_dir": target.display().to_string() }).to_string()),
        parallel: None,
        max_agents: None,
        max_cost: req.max_cost,
        budget: None,
        run_dir: None,
        resume: None,
        watchdog: None,
        parent: req.parent,
    })
}

/// Which evaluator governs this call — the same two-rung shape the pre-agent
/// policy hook uses (`ccteam_im::policy::resolve_hook`: the project's file
/// first, the ccteam home second, the file itself IS the registration, a stat
/// per rung with no caching). A project that states an evaluator states all of
/// it: the project file *replaces* the global one, never merges with it.
fn resolve_evaluator(paths: &CcteamPaths, project_dir: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(dir) = project_dir {
        let project_rung = project_flows_dir(&dir).join(EVAL_SCRIPT_FILE);
        if project_rung.is_file() {
            return Ok(project_rung);
        }
    }
    let global_rung = paths.root.join(FLOWS_DIR).join(EVAL_SCRIPT_FILE);
    if global_rung.is_file() {
        return Ok(global_rung);
    }
    Err(anyhow!(
        "no evaluator script — copy examples/flows/flow-review.flow.js to \
         `.agents/flows/{EVAL_SCRIPT_FILE}` (or `{}`) and adjust, \
         then rerun `ccteam flow eval`",
        global_rung.display()
    ))
}

/// The project whose `.agents/flows/` is the first rung: the named slug, else
/// whichever project the cwd sits in. `None` when neither answers — the global
/// rung then has to carry the call, exactly as it does for remote projects
/// whose `.ccteam/` lives on another machine.
fn project_dir_for(paths: &CcteamPaths, slug: Option<&str>) -> Option<PathBuf> {
    if let Some(slug) = slug {
        return Some(paths.project_dir(slug));
    }
    let cwd = std::env::current_dir().ok()?;
    ccteam_core::session_context_from_cwd(&cwd, paths)
        .ok()
        .map(|context| context.project_dir)
}

/// The run under review: a path when it looks like one, a bare run id
/// otherwise. The id is what `flow run` already named the directory under
/// `<ccteam home>/runs/`, and retyping the whole path to review the run you
/// just made is pure friction. Always resolved to an absolute path — the
/// evaluating agent reads it from its own cwd, not from this shell's.
fn resolve_eval_target(paths: &CcteamPaths, run: &str) -> Result<PathBuf> {
    // A bare id has no separators. It means "the run `flow run` named under
    // `runs/`" and is resolved THERE FIRST: a same-named directory in the cwd
    // must not shadow the id the user was handed (checker s523 R1). A cwd
    // directory that happens to be a run is still reachable — `./name` says
    // "path" unambiguously.
    let bare = !run.is_empty() && !run.contains('/') && !run.contains('\\');
    if bare {
        let under_runs = paths.root.join(RUNS_DIR).join(run);
        if is_run_dir(&under_runs) {
            return std::fs::canonicalize(&under_runs)
                .with_context(|| format!("resolve run directory {}", under_runs.display()));
        }
    }
    let as_given = PathBuf::from(run);
    if is_run_dir(&as_given) {
        return std::fs::canonicalize(&as_given)
            .with_context(|| format!("resolve run directory {}", as_given.display()));
    }
    if bare {
        return Err(anyhow!(
            "no workflow run at {} or {} (a run directory is one that holds journal.jsonl)",
            paths.root.join(RUNS_DIR).join(run).display(),
            as_given.display(),
        ));
    }
    Err(anyhow!(
        "{} is not a workflow run directory (no journal.jsonl)",
        as_given.display()
    ))
}

/// The workspace the cwd belongs to, read from the `.ccteam/state.json` that
/// `ccteam init` wrote. Never guessed from the path itself.
fn project_from_cwd(paths: &CcteamPaths) -> Result<String> {
    let cwd = std::env::current_dir().context("resolve current directory")?;
    let context = ccteam_core::session_context_from_cwd(&cwd, paths).with_context(|| {
        format!(
            "cannot tell which project this workflow runs in from {} — pass `--project <slug>`",
            cwd.display()
        )
    })?;
    Ok(context.slug)
}

/// Decide where the journal lives, and whether we are continuing an old run.
///
/// `--resume` and `--run-dir` are mutually exclusive in clap; this only has to
/// turn whichever was given into a directory that exists.
fn resolve_run_dir(
    paths: &CcteamPaths,
    script: &Path,
    run_dir: Option<PathBuf>,
    resume: Option<PathBuf>,
) -> Result<(PathBuf, bool)> {
    if let Some(dir) = resume {
        if !is_run_dir(&dir) {
            return Err(anyhow!(
                "{} is not a workflow run directory (no journal.jsonl)",
                dir.display()
            ));
        }
        return Ok((dir, true));
    }
    if let Some(dir) = run_dir {
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("create run directory {}", dir.display()))?;
        // An explicit --run-dir that already holds a journal is a continuation
        // by intent: re-running the same directory must not silently re-hire
        // everything the journal already paid for.
        let resuming = is_run_dir(&dir);
        return Ok((dir, resuming));
    }
    let root = paths.root.join(RUNS_DIR);
    let stem = script
        .file_stem()
        .and_then(|s| s.to_str())
        .map(sanitize_stem)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "workflow".to_string());
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    // Two runs of the same script in the same second must not share a journal.
    for attempt in 0..100 {
        let name = if attempt == 0 {
            format!("{stem}-{stamp}")
        } else {
            format!("{stem}-{stamp}-{attempt}")
        };
        let dir = root.join(name);
        match std::fs::create_dir_all(&dir) {
            Ok(()) if dir.read_dir().is_ok_and(|mut e| e.next().is_none()) => {
                return Ok((dir, false))
            }
            Ok(()) => continue,
            Err(err) => {
                return Err(anyhow!("create run directory {}: {err}", dir.display()));
            }
        }
    }
    Err(anyhow!(
        "could not find a free run directory under {}",
        root.display()
    ))
}

/// Keep a directory name to what a shell and a human both read easily.
fn sanitize_stem(stem: &str) -> String {
    stem.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

/// One plain line per event on stderr. No TUI: this has to be readable in a
/// log file, a CI transcript and a pipe, and those three rule out redrawing.
fn stderr_progress() -> ProgressCallback {
    Arc::new(|event| eprintln!("{}", render(&event)))
}

fn render(event: &ProgressEvent) -> String {
    match event {
        ProgressEvent::RunStarted {
            name,
            description,
            phases,
        } => {
            let mut line = format!("run {name} — {description}");
            if !phases.is_empty() {
                line.push_str(&format!(" | phases: {}", phases.join(" → ")));
            }
            line
        }
        ProgressEvent::PhaseStarted { title } => format!("phase {title}"),
        ProgressEvent::AgentStarted {
            seq,
            label,
            vendor,
            cached,
        } => {
            let vendor = vendor.map(|v| v.wire_name()).unwrap_or("default");
            let cached = if *cached { " (cached)" } else { "" };
            format!("agent #{seq} start [{vendor}] {label}{cached}")
        }
        ProgressEvent::AgentFinished {
            seq,
            label,
            outcome,
            cost_usd,
        } => {
            let verdict = match outcome {
                None => "null".to_string(),
                Some(o) if o.failed => format!(
                    "failed ({})",
                    o.error_kind.as_deref().unwrap_or("no reason given")
                ),
                Some(_) => "ok".to_string(),
            };
            format!("agent #{seq} done  {label} — {verdict} ${cost_usd:.4}")
        }
        ProgressEvent::Log { message, phase } => match phase {
            Some(phase) => format!("log [{phase}] {message}"),
            None => format!("log {message}"),
        },
        ProgressEvent::BrakeTripped { reason } => format!("brake {reason}"),
        ProgressEvent::RunFinished {
            agents,
            cost_usd,
            ok,
        } => {
            let verdict = if *ok { "ok" } else { "not ok" };
            format!("run finished — {agents} agent(s), ${cost_usd:.4}, {verdict}")
        }
    }
}

/// The machine-readable half: one line of compact JSON on stdout.
fn report_json(report: &RunReport) -> String {
    let agents: Vec<Value> = report
        .agents
        .iter()
        .map(|a| {
            json!({
                "seq": a.seq,
                "key": a.key,
                "label": a.label,
                "phase": a.phase,
                "vendor": a.vendor.map(|v| v.wire_name()),
                "sid": a.sid,
                "cost_usd": a.cost_usd,
                "cached": a.cached,
                "ok": a.ok,
                "error": a.error,
            })
        })
        .collect();
    json!({
        "name": report.meta.name,
        "description": report.meta.description,
        "ok": report.ok(),
        "run_dir": report.run_dir.display().to_string(),
        "returned": report.returned,
        "totals": {
            "agents": report.totals.agents,
            "cost_usd": report.totals.cost_usd,
        },
        "brake": report.brake,
        "script_error": report.script_error,
        "cache": {
            "hits": report.cache.hits,
            "reattached": report.cache.reattached,
            "invalidated_at": report.cache.invalidated_at,
            "diagnostic": report.cache.diagnostic,
        },
        "agents": agents,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    #[test]
    fn parent_precedence_flag_env_blank() {
        use super::resolve_parent;
        assert_eq!(
            resolve_parent(Some("s9".into()), Some("s487".into())),
            Some("s9".into())
        );
        assert_eq!(
            resolve_parent(None, Some("s487".into())),
            Some("s487".into())
        );
        assert_eq!(resolve_parent(None, Some("  ".into())), None);
        assert_eq!(resolve_parent(None, None), None);
    }

    use super::*;
    use ccteam_flow::AgentOutcome;

    fn paths_in(root: &Path) -> CcteamPaths {
        CcteamPaths {
            root: root.join("home"),
            projects_root: root.join("projects"),
        }
    }

    #[test]
    fn the_default_run_dir_lands_under_the_canonical_runs_home() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = paths_in(tmp.path());
        let (dir, resuming) =
            resolve_run_dir(&paths, Path::new("/w/review.js"), None, None).unwrap();
        assert!(!resuming);
        assert_eq!(dir.parent().unwrap(), paths.root.join(RUNS_DIR));
        assert!(
            dir.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("review-"),
            "{dir:?}"
        );
        assert!(
            ccteam_core::canonical_home_dirs().contains(&RUNS_DIR),
            "the runs home must be canonical or `ccteam doctor` reports drift",
        );
    }

    /// Two runs of the same script in the same second get their own journals —
    /// sharing one would make the second look like a resume of the first.
    #[test]
    fn a_second_run_of_the_same_script_gets_its_own_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = paths_in(tmp.path());
        let (first, _) = resolve_run_dir(&paths, Path::new("review.js"), None, None).unwrap();
        std::fs::write(first.join("journal.jsonl"), "").unwrap();
        let (second, _) = resolve_run_dir(&paths, Path::new("review.js"), None, None).unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn resume_demands_a_real_run_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = paths_in(tmp.path());
        let empty = tmp.path().join("not-a-run");
        std::fs::create_dir_all(&empty).unwrap();
        let err =
            resolve_run_dir(&paths, Path::new("review.js"), None, Some(empty.clone())).unwrap_err();
        assert!(err.to_string().contains("journal.jsonl"), "{err}");

        std::fs::write(empty.join("journal.jsonl"), "").unwrap();
        let (dir, resuming) =
            resolve_run_dir(&paths, Path::new("review.js"), None, Some(empty.clone())).unwrap();
        assert_eq!(dir, empty);
        assert!(resuming, "--resume must read the journal as a cache");
    }

    /// Re-pointing `--run-dir` at a journal is a continuation by intent: the
    /// alternative is silently paying twice for every call it already holds.
    #[test]
    fn an_explicit_run_dir_with_a_journal_resumes() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = paths_in(tmp.path());
        let dir = tmp.path().join("mine");
        let (_, fresh) =
            resolve_run_dir(&paths, Path::new("r.js"), Some(dir.clone()), None).unwrap();
        assert!(!fresh);
        std::fs::write(dir.join("journal.jsonl"), "").unwrap();
        let (_, resuming) = resolve_run_dir(&paths, Path::new("r.js"), Some(dir), None).unwrap();
        assert!(resuming);
    }

    #[test]
    fn progress_lines_are_one_line_each_and_name_the_agent() {
        let started = render(&ProgressEvent::AgentStarted {
            seq: 3,
            label: "review the diff".into(),
            vendor: Some(ccteam_harness::AgentVendor::Codex),
            cached: false,
        });
        assert_eq!(started, "agent #3 start [codex] review the diff");
        let finished = render(&ProgressEvent::AgentFinished {
            seq: 3,
            label: "review the diff".into(),
            outcome: Some(AgentOutcome::text("looks fine")),
            cost_usd: 0.125,
        });
        assert!(finished.contains("ok"), "{finished}");
        assert!(finished.contains("$0.1250"), "{finished}");
        for line in [started, finished] {
            assert!(!line.contains('\n'), "one event, one line: {line}");
        }
    }

    /// A failed call must say WHY on the progress line — "null" alone sends
    /// the operator to the journal for something the run already knew.
    #[test]
    fn a_failed_agent_line_carries_its_reason() {
        let line = render(&ProgressEvent::AgentFinished {
            seq: 1,
            label: "build".into(),
            outcome: Some(AgentOutcome {
                failed: true,
                error_kind: Some("server_overloaded".into()),
                ..AgentOutcome::default()
            }),
            cost_usd: 0.0,
        });
        assert!(line.contains("server_overloaded"), "{line}");
    }

    #[test]
    fn a_stem_with_shell_metacharacters_cannot_escape_the_runs_dir() {
        assert_eq!(sanitize_stem("../../etc/passwd"), "etc-passwd");
        assert_eq!(sanitize_stem("re view;rm -rf"), "re-view-rm--rf");
    }

    /// The user-global evaluator rung has to be part of the home manifest or
    /// `ccteam doctor` reports it as an orchestrator-era leftover to delete.
    #[test]
    fn the_flows_home_is_canonical() {
        assert!(
            ccteam_core::canonical_home_dirs().contains(&FLOWS_DIR),
            "the flows home must be canonical or `ccteam doctor` reports drift",
        );
    }

    #[test]
    fn the_scaffold_names_the_flow_and_never_clobbers() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(".agents").join("flows");
        let file = write_scaffold(&dir, "audit-routes").unwrap();
        assert_eq!(file.file_name().unwrap(), "audit-routes.flow.js");
        let body = std::fs::read_to_string(&file).unwrap();
        assert!(body.contains("name: 'audit-routes'"), "{body}");
        assert!(body.contains("return null"), "{body}");

        std::fs::write(&file, "// mine, hand-written").unwrap();
        let err = write_scaffold(&dir, "audit-routes").unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "// mine, hand-written",
            "a refused scaffold must not have touched the file",
        );
    }

    /// The scaffold is code, not content: `meta` plus comments. Even a
    /// placeholder `agent('TODO: …')` is a prompt string an unedited run would
    /// send to a model (checker s523 R1), so the template must carry NO agent
    /// call at all — the template is the one place prompt content could creep
    /// into this repo unnoticed.
    #[test]
    fn the_scaffold_carries_no_prompt_content() {
        let body = script_template("x");
        assert!(body.contains("TODO"), "{body}");
        for line in body.lines() {
            assert!(line.len() < 90, "template line too long to be code: {line}");
        }
        assert_eq!(
            body.matches("agent(").count(),
            1,
            "agent() may appear once, in the globals comment only: {body}"
        );
        let comments_only: String = body
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect();
        assert!(
            !comments_only.contains("agent("),
            "no agent call outside comments: {body}"
        );
    }

    /// The cheat sheet IS the authoring manual — an agent that ran `flow new`
    /// should not have to open the docs to write a correct first flow.
    #[test]
    fn the_cheat_sheet_covers_the_whole_script_surface() {
        for global in [
            "agent(task, opts?)",
            "parallel(",
            "pipeline(",
            "phase(title)",
            "log(m)",
            "args",
            "budget",
            "usage()",
        ] {
            assert!(CHEAT_SHEET.contains(global), "cheat sheet lost {global}");
        }
        // The two distinctions a first-time author gets wrong.
        assert!(CHEAT_SHEET.contains("null"), "null-vs-throw must be stated");
        assert!(
            CHEAT_SHEET.contains("throws"),
            "null-vs-throw must be stated"
        );
        assert!(CHEAT_SHEET.contains("retry:{max,prompt}"));
    }

    fn seed_run(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("journal.jsonl"), "").unwrap();
    }

    #[test]
    fn eval_takes_a_bare_run_id_under_the_runs_home() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = paths_in(tmp.path());
        let run = paths.root.join(RUNS_DIR).join("review-20260901-101010");
        seed_run(&run);
        assert_eq!(
            resolve_eval_target(&paths, "review-20260901-101010").unwrap(),
            run
        );
    }

    #[test]
    fn eval_takes_a_path_and_absolutizes_it_for_the_agent() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = paths_in(tmp.path());
        let run = tmp.path().join("elsewhere");
        seed_run(&run);
        let resolved = resolve_eval_target(&paths, run.to_str().unwrap()).unwrap();
        assert!(resolved.is_absolute(), "{resolved:?}");
        assert!(is_run_dir(&resolved));
    }

    /// A directory without a journal is not a run — and the error has to name
    /// both places that were tried, or a typo'd id reads as "no such run".
    #[test]
    fn eval_refuses_a_directory_that_is_not_a_run() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = paths_in(tmp.path());
        std::fs::create_dir_all(tmp.path().join("empty")).unwrap();
        let err = resolve_eval_target(&paths, "no-such-run").unwrap_err();
        assert!(err.to_string().contains("no-such-run"), "{err}");
        assert!(err.to_string().contains(RUNS_DIR), "{err}");

        let err =
            resolve_eval_target(&paths, tmp.path().join("empty").to_str().unwrap()).unwrap_err();
        assert!(err.to_string().contains("journal.jsonl"), "{err}");
    }

    /// Same two-rung shape as the pre-agent policy hook: the project's file
    /// REPLACES the global one, and neither existing is a named error that
    /// tells the operator what to copy where.
    #[test]
    fn the_evaluator_resolves_project_first_then_home() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = paths_in(tmp.path());
        let project = tmp.path().join("myapp");
        let global = paths.root.join(FLOWS_DIR).join(EVAL_SCRIPT_FILE);

        let err = resolve_evaluator(&paths, Some(project.clone())).unwrap_err();
        assert!(err.to_string().contains("flow-review.flow.js"), "{err}");
        assert!(err.to_string().contains(EVAL_SCRIPT_FILE), "{err}");

        std::fs::create_dir_all(global.parent().unwrap()).unwrap();
        std::fs::write(&global, "").unwrap();
        assert_eq!(
            resolve_evaluator(&paths, Some(project.clone())).unwrap(),
            global
        );

        let project_rung = project_flows_dir(&project).join(EVAL_SCRIPT_FILE);
        std::fs::create_dir_all(project_rung.parent().unwrap()).unwrap();
        std::fs::write(&project_rung, "").unwrap();
        assert_eq!(
            resolve_evaluator(&paths, Some(project)).unwrap(),
            project_rung,
            "the project's evaluator replaces the global one, never merges",
        );
    }

    /// `.agents/flows/` — not `.ccteam/`, which ccteam gitignores.
    #[test]
    fn project_flows_live_where_git_can_see_them() {
        let dir = project_flows_dir(Path::new("/w/myapp"));
        assert!(dir.ends_with(".agents/flows"), "{dir:?}");
        assert!(!dir.to_str().unwrap().contains(".ccteam"), "{dir:?}");
    }
}
