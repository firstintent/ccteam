//! M2.1: sub-skill scheduling.
//!
//! Phase YAML's `sub_skills:` list (interfaces §7) names plugin agents
//! that the orchestrator runs around phase boundaries:
//! - `phase_start` — fires before the phase prompt is injected;
//! - `phase_done` — fires after `PHASE_DONE: <name>` is parsed,
//!   before the orchestrator advances to the next phase.
//!
//! Output: the spawned subprocess writes its result to disk at
//! `<project_dir>/<output_to>`. The next phase's prompt
//! [`super::progress::build_phase_prompt_with_attachments`] picks the
//! file up via `@`-reference at injection time, so phase markdown
//! doesn't need to know which sub-skills ran.
//!
//! Subprocess shape (production): a `claude -p --output-format text`
//! invocation that reads the resolved plugin agent path via `@`. The
//! orchestrator does **not** embed an LLM in-process — that would
//! violate the Symphony anti-pattern (tech-design §3.1). All LLM work
//! still lives in claude.
//!
//! Path-prefix resolution mirrors interfaces §7.3:
//! - `claude-plugins-official:<plugin>/<rel>` → resolves under
//!   `~/.claude/plugins/marketplaces/claude-plugins-official/plugins/`;
//! - `local:<rel>` → resolved under the project dir;
//! - `installed:<plugin>/<command>` → reserved for M3 plugin install.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{anyhow, Context, Result};
use chrono::{SecondsFormat, Utc};
use serde_json::json;

use crate::phases::{PhaseTemplate, SubSkillSpec, SubSkillTrigger};
use crate::progress::append_event;
use crate::tool_surface::user_claude_dir;

/// Strategy abstraction so tests can stub out subprocess execution
/// without a real `claude` on the PATH. Production wiring uses
/// [`ClaudePRunner`].
pub trait SubSkillRunner: Send + Sync {
    /// Run a sub-skill and return the body to be written to
    /// `output_to`. Implementations must NOT touch the filesystem
    /// outside reading the `agent_path`; the orchestrator owns the
    /// `output_to` write so a runner failure doesn't leave a half-
    /// written file behind.
    fn run(&self, ctx: &SubSkillRunCtx<'_>) -> Result<String>;
}

/// Argument bundle for [`SubSkillRunner::run`]. Borrowed so callers
/// don't pay a clone per invocation.
#[derive(Debug)]
pub struct SubSkillRunCtx<'a> {
    pub project_dir: &'a Path,
    pub phase_name: &'a str,
    pub trigger: SubSkillTrigger,
    /// Resolved on-disk path to the plugin agent / hook script.
    /// `None` when the prefix didn't resolve (caller decides
    /// whether to bail or skip).
    pub agent_path: Option<PathBuf>,
    /// Verbatim `skill:` string from the phase YAML. Useful for
    /// runners that want to embed the original reference.
    pub skill: &'a str,
}

/// Outcome of running one sub-skill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubSkillOutcome {
    /// Agent ran and produced output written to `output_to`.
    Ran { output: PathBuf },
    /// Skill prefix didn't resolve to any source we know how to run
    /// (e.g. `installed:<plugin>/...` with no plugin command map yet).
    /// Logged + recorded in progress.jsonl, but not fatal — phases
    /// stay advisory.
    Skipped { reason: String },
    /// Runner returned an error. Logged + recorded; phase continues
    /// because sub-skills are advisory (interfaces §7).
    Failed { error: String },
}

/// Production runner: spawns `claude -p --output-format text`
/// (configurable for tests via [`ClaudePRunner::with_argv`]).
///
/// The runner accepts the agent path on stdin via `@`-prefixed text
/// so claude inlines the agent's body before evaluating. Output is
/// captured from stdout; non-zero exit = runner error.
pub struct ClaudePRunner {
    argv: Vec<String>,
}

impl Default for ClaudePRunner {
    fn default() -> Self {
        Self {
            // `--dangerously-skip-permissions` here lets the sub-skill
            // run without prompts — same posture as the orchestrator's
            // tmux session; the sub-skill is short-lived and doesn't
            // need a separate trust dialog.
            argv: vec![
                "claude".into(),
                "-p".into(),
                "--output-format".into(),
                "text".into(),
                "--dangerously-skip-permissions".into(),
            ],
        }
    }
}

impl ClaudePRunner {
    /// Override the spawned argv. Tests pass a shell stub like
    /// `["bash", "-c", "cat"]` that echoes stdin to stdout.
    pub fn with_argv(argv: Vec<String>) -> Self {
        Self { argv }
    }
}

impl SubSkillRunner for ClaudePRunner {
    fn run(&self, ctx: &SubSkillRunCtx<'_>) -> Result<String> {
        let Some(agent_path) = &ctx.agent_path else {
            return Err(anyhow!(
                "sub-skill `{}` did not resolve to an agent path; cannot run",
                ctx.skill,
            ));
        };
        let prompt = build_subskill_prompt(ctx.phase_name, ctx.trigger, agent_path);
        let mut cmd = Command::new(&self.argv[0]);
        for arg in &self.argv[1..] {
            cmd.arg(arg);
        }
        cmd.current_dir(ctx.project_dir);
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let mut child = cmd.spawn().with_context(|| {
            format!("spawn sub-skill runner {:?}", self.argv)
        })?;
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            stdin
                .write_all(prompt.as_bytes())
                .context("write sub-skill prompt to runner stdin")?;
        }
        let output = child
            .wait_with_output()
            .context("wait for sub-skill runner")?;
        if !output.status.success() {
            return Err(anyhow!(
                "sub-skill runner exited {:?}: stderr={}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr),
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

/// Build the prompt fed to the runner's stdin. Single source of truth
/// for the wording so test stubs and the production path stay aligned
/// on what the agent sees.
fn build_subskill_prompt(phase: &str, trigger: SubSkillTrigger, agent_path: &Path) -> String {
    let when = match trigger {
        SubSkillTrigger::PhaseStart => "phase_start",
        SubSkillTrigger::PhaseDone => "phase_done",
    };
    format!(
        "你被 ccteam orchestrator 在 phase=`{phase}` trigger=`{when}` 时触发。\n\n请按下面 agent 的指令工作,把成果直接打印到 stdout:\n\n@{}\n",
        agent_path.display(),
    )
}

/// Resolve a `skill:` reference (interfaces §7.3) to an on-disk path.
/// Returns `None` for prefixes the orchestrator does not yet know how
/// to run (`installed:`); the caller logs a `Skipped` outcome so the
/// operator sees what was missed.
pub fn resolve_skill_path(skill: &str, project_dir: &Path) -> Option<PathBuf> {
    if let Some(rest) = skill.strip_prefix("claude-plugins-official:") {
        // `claude-plugins-official:<plugin>/<rel>` →
        // `~/.claude/plugins/marketplaces/claude-plugins-official/plugins/<plugin>/<rel>`
        let claude = user_claude_dir().ok()?;
        let mut parts = rest.splitn(2, '/');
        let plugin = parts.next()?;
        let rel = parts.next()?;
        let mut p = claude
            .join("plugins")
            .join("marketplaces")
            .join("claude-plugins-official")
            .join("plugins")
            .join(plugin);
        for seg in rel.split('/') {
            p = p.join(seg);
        }
        // Append `.md` if the caller omitted it (the phase YAML form
        // can drop the suffix when the path obviously names an agent).
        if !p.exists() {
            let with_md = p.with_extension("md");
            if with_md.exists() {
                return Some(with_md);
            }
        }
        Some(p)
    } else if let Some(rest) = skill.strip_prefix("local:") {
        let mut p = project_dir.to_path_buf();
        for seg in rest.split('/') {
            p = p.join(seg);
        }
        Some(p)
    } else if skill.starts_with("installed:") {
        // M3 territory — return None so the caller emits `Skipped`.
        None
    } else {
        None
    }
}

/// Run every sub-skill in `template.sub_skills` whose trigger matches
/// `trigger`, writing each one's stdout to its `output_to`. Skill
/// failures land in progress.jsonl but do not abort the phase — they
/// are advisory by design (interfaces §7).
///
/// Returns the list of `output_to` paths that were successfully
/// written (the orchestrator passes these to
/// [`super::progress::build_phase_prompt_with_attachments`] when
/// dispatching the next phase).
pub fn run_sub_skills_for_phase(
    template: &PhaseTemplate,
    trigger: SubSkillTrigger,
    project_dir: &Path,
    progress_path: &Path,
    runner: &dyn SubSkillRunner,
) -> Vec<PathBuf> {
    let mut outputs = Vec::new();
    for spec in &template.sub_skills {
        if spec.trigger != trigger {
            continue;
        }
        match run_one(spec, &template.name, project_dir, progress_path, runner) {
            Ok(SubSkillOutcome::Ran { output }) => outputs.push(output),
            Ok(SubSkillOutcome::Skipped { reason }) => {
                tracing::info!(skill = %spec.skill, %reason, "sub-skill skipped");
            }
            Ok(SubSkillOutcome::Failed { error }) => {
                tracing::warn!(skill = %spec.skill, error = %error, "sub-skill failed");
            }
            Err(err) => tracing::warn!(
                skill = %spec.skill,
                error = %err,
                "sub-skill bookkeeping failed",
            ),
        }
    }
    outputs
}

fn run_one(
    spec: &SubSkillSpec,
    phase: &str,
    project_dir: &Path,
    progress_path: &Path,
    runner: &dyn SubSkillRunner,
) -> Result<SubSkillOutcome> {
    let agent_path = resolve_skill_path(&spec.skill, project_dir);
    let _ = append_event(
        progress_path,
        &json!({
            "ts": now_rfc3339(),
            "event": "subskill_started",
            "phase": phase,
            "skill": spec.skill,
            "trigger": trigger_str(spec.trigger),
        }),
    );
    if agent_path.is_none() && !spec.skill.starts_with("local:") {
        let reason = format!("unsupported skill prefix: {}", spec.skill);
        let _ = append_event(
            progress_path,
            &json!({
                "ts": now_rfc3339(),
                "event": "subskill_skipped",
                "phase": phase,
                "skill": spec.skill,
                "reason": reason.clone(),
            }),
        );
        return Ok(SubSkillOutcome::Skipped { reason });
    }
    let ctx = SubSkillRunCtx {
        project_dir,
        phase_name: phase,
        trigger: spec.trigger,
        agent_path,
        skill: &spec.skill,
    };
    let body = match runner.run(&ctx) {
        Ok(b) => b,
        Err(err) => {
            let msg = format!("{err:#}");
            let _ = append_event(
                progress_path,
                &json!({
                    "ts": now_rfc3339(),
                    "event": "subskill_failed",
                    "phase": phase,
                    "skill": spec.skill,
                    "error": msg.clone(),
                }),
            );
            return Ok(SubSkillOutcome::Failed { error: msg });
        }
    };
    let output_path = project_dir.join(&spec.output_to);
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::write(&output_path, body.as_bytes())
        .with_context(|| format!("write sub-skill output {}", output_path.display()))?;
    let _ = append_event(
        progress_path,
        &json!({
            "ts": now_rfc3339(),
            "event": "subskill_done",
            "phase": phase,
            "skill": spec.skill,
            "output": spec.output_to,
            "bytes": body.len(),
        }),
    );
    Ok(SubSkillOutcome::Ran {
        output: output_path,
    })
}

fn trigger_str(t: SubSkillTrigger) -> &'static str {
    match t {
        SubSkillTrigger::PhaseStart => "phase_start",
        SubSkillTrigger::PhaseDone => "phase_done",
    }
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stub runner: writes a fixed string + records every call so
    /// tests can assert how many sub-skills ran and with what
    /// arguments.
    struct StubRunner {
        body: String,
    }

    impl StubRunner {
        fn new(body: impl Into<String>) -> Self {
            Self { body: body.into() }
        }
    }

    impl SubSkillRunner for StubRunner {
        fn run(&self, _ctx: &SubSkillRunCtx<'_>) -> Result<String> {
            Ok(self.body.clone())
        }
    }

    fn template_with_sub_skills(skills: Vec<SubSkillSpec>) -> PhaseTemplate {
        let yaml = "name: implement\nparallelism: solo\n";
        let mut t = PhaseTemplate::parse(&format!("---\n{yaml}---\nbody\n")).unwrap();
        t.sub_skills = skills;
        t
    }

    #[test]
    fn resolve_local_prefix_joins_project_dir() {
        let p = resolve_skill_path("local:scripts/foo.sh", Path::new("/proj")).unwrap();
        assert_eq!(p, PathBuf::from("/proj/scripts/foo.sh"));
    }

    #[test]
    fn resolve_installed_prefix_returns_none() {
        assert!(resolve_skill_path("installed:foo/bar", Path::new("/p")).is_none());
    }

    #[test]
    fn resolve_unprefixed_skill_returns_none() {
        // Don't try to guess for unrecognized prefixes — the M2.1
        // grammar is closed (interfaces §7.3), and a typo'd prefix
        // should land as Skipped, not silently looked up under some
        // default.
        assert!(resolve_skill_path("plugin-name/agents/foo", Path::new("/p")).is_none());
    }

    #[test]
    fn run_sub_skills_writes_output_for_matching_trigger() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project = tmp.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let progress = tmp.path().join("progress.jsonl");
        let template = template_with_sub_skills(vec![SubSkillSpec {
            skill: "local:agent.md".into(),
            trigger: SubSkillTrigger::PhaseDone,
            output_to: ".ccteam/code-review.md".into(),
        }]);
        // Stage the local agent file so resolve succeeds.
        std::fs::write(project.join("agent.md"), "# stub agent\n").unwrap();
        let runner = StubRunner::new("REVIEW BODY\n");

        let outs = run_sub_skills_for_phase(
            &template,
            SubSkillTrigger::PhaseDone,
            &project,
            &progress,
            &runner,
        );
        assert_eq!(outs.len(), 1);
        let body = std::fs::read_to_string(project.join(".ccteam/code-review.md")).unwrap();
        assert_eq!(body, "REVIEW BODY\n");
        // progress.jsonl recorded started + done.
        let evs = std::fs::read_to_string(&progress).unwrap();
        assert!(evs.contains("subskill_started"));
        assert!(evs.contains("subskill_done"));
    }

    #[test]
    fn run_sub_skills_ignores_other_triggers() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project = tmp.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let progress = tmp.path().join("progress.jsonl");
        let template = template_with_sub_skills(vec![SubSkillSpec {
            skill: "local:agent.md".into(),
            trigger: SubSkillTrigger::PhaseStart,
            output_to: ".ccteam/precheck.md".into(),
        }]);
        std::fs::write(project.join("agent.md"), "x").unwrap();
        let runner = StubRunner::new("X");

        let outs = run_sub_skills_for_phase(
            &template,
            SubSkillTrigger::PhaseDone, // mismatched
            &project,
            &progress,
            &runner,
        );
        assert!(outs.is_empty(), "phase_start spec must not run on phase_done");
        assert!(!project.join(".ccteam/precheck.md").exists());
    }

    #[test]
    fn run_sub_skills_records_skipped_for_installed_prefix() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project = tmp.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let progress = tmp.path().join("progress.jsonl");
        let template = template_with_sub_skills(vec![SubSkillSpec {
            skill: "installed:foo/bar".into(),
            trigger: SubSkillTrigger::PhaseDone,
            output_to: ".ccteam/foo.md".into(),
        }]);
        let runner = StubRunner::new("body");
        let outs = run_sub_skills_for_phase(
            &template,
            SubSkillTrigger::PhaseDone,
            &project,
            &progress,
            &runner,
        );
        assert!(outs.is_empty(), "installed: prefix should be skipped (not run)");
        let evs = std::fs::read_to_string(&progress).unwrap();
        assert!(evs.contains("subskill_skipped"));
    }

    #[test]
    fn run_sub_skills_records_failure_when_runner_errors() {
        struct ErrRunner;
        impl SubSkillRunner for ErrRunner {
            fn run(&self, _: &SubSkillRunCtx<'_>) -> Result<String> {
                Err(anyhow!("boom"))
            }
        }
        let tmp = tempfile::TempDir::new().unwrap();
        let project = tmp.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let progress = tmp.path().join("progress.jsonl");
        let template = template_with_sub_skills(vec![SubSkillSpec {
            skill: "local:agent.md".into(),
            trigger: SubSkillTrigger::PhaseDone,
            output_to: ".ccteam/x.md".into(),
        }]);
        std::fs::write(project.join("agent.md"), "x").unwrap();
        let outs = run_sub_skills_for_phase(
            &template,
            SubSkillTrigger::PhaseDone,
            &project,
            &progress,
            &ErrRunner,
        );
        assert!(outs.is_empty(), "failed runner must not produce an output path");
        let evs = std::fs::read_to_string(&progress).unwrap();
        assert!(evs.contains("subskill_failed"));
    }

    #[test]
    fn build_subskill_prompt_includes_at_reference() {
        let p = build_subskill_prompt(
            "implement",
            SubSkillTrigger::PhaseDone,
            Path::new("/some/agent.md"),
        );
        assert!(p.contains("phase=`implement`"));
        assert!(p.contains("trigger=`phase_done`"));
        assert!(p.contains("@/some/agent.md"));
    }
}
