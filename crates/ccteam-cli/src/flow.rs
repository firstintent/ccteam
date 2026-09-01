//! `ccteam flow run <file.js>` — drive one dynamic workflow.
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
        if !dir.join("journal.jsonl").is_file() {
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
        let resuming = dir.join("journal.jsonl").is_file();
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
}
