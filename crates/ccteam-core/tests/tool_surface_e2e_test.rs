//! M0.5.7 — end-to-end regression for the tool-surface foundation.
//!
//! Acceptance: a freshly bootstrapped project must come up under
//! orchestrator startup even when its phase markdown declares
//! `tools_required: subagents: [code-reviewer]`. The bootstrap path
//! is responsible for ln -sf'ing the agent file before
//! `Orchestrator::new`'s tool-surface validator runs.
//!
//! We exercise the full chain — bootstrap_project (which calls
//! setup_tool_surface internally) → orchestrator construction with
//! the real `tools_required` from `phases/03-implement.md` — under
//! an isolated `CLAUDE_CONFIG_HOME` pointed at a tempdir so the test
//! doesn't depend on the developer's `~/.claude/` state.

use std::path::Path;
use std::sync::Mutex;

use chrono::{SecondsFormat, Utc};
use serde_json::json;
use tempfile::TempDir;

use ccteam_core::{
    bootstrap_project, decide_tick_from_events, dev_dag,
    link_recommended_agents_for_phases_into, progress, slugify, AgentLinkAction, CcteamPaths,
    LinkOptions, Orchestrator, OrchestratorConfig, PhaseState, PhaseTemplate, ProjectState,
    TickAction, RECOMMENDED_AGENTS,
};

/// Several tests in this file mutate `CLAUDE_CONFIG_HOME`, which is a
/// process-global. Cargo runs tests inside one binary in parallel by
/// default, so we serialize through one mutex per binary.
static ENV_GUARD: Mutex<()> = Mutex::new(());

struct ScopedClaude<'a> {
    _lock: std::sync::MutexGuard<'a, ()>,
    prior: Option<String>,
}

impl<'a> ScopedClaude<'a> {
    fn set(value: &Path) -> Self {
        let lock = ENV_GUARD.lock().unwrap();
        let prior = std::env::var("CLAUDE_CONFIG_HOME").ok();
        std::env::set_var("CLAUDE_CONFIG_HOME", value);
        Self { _lock: lock, prior }
    }
}

impl Drop for ScopedClaude<'_> {
    fn drop(&mut self) {
        match &self.prior {
            Some(v) => std::env::set_var("CLAUDE_CONFIG_HOME", v),
            None => std::env::remove_var("CLAUDE_CONFIG_HOME"),
        }
    }
}

fn stage_plugin_sources(claude_dir: &Path) {
    for agent in RECOMMENDED_AGENTS {
        let src = agent.source_path(claude_dir);
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::write(&src, format!("# stub for {}\n", agent.filename)).unwrap();
    }
}

fn write_global_phases_with_implement(phases_dir: &Path) {
    std::fs::create_dir_all(phases_dir).unwrap();
    // The shipped 03-implement.md declares tools_required:[code-reviewer]
    // — we read it from disk so this test breaks loudly if anyone
    // weakens the declaration.
    let implement_src = include_str!("../../../teams/dev/phases/03-implement.md");
    std::fs::write(phases_dir.join("03-implement.md"), implement_src).unwrap();
    // Plus a minimal plan-eng so the orchestrator has more than one
    // template to validate.
    let plan_eng = concat!(
        "---\n",
        "name: plan-eng\n",
        "parallelism: solo\n",
        "---\n",
        "body\n",
    );
    std::fs::write(phases_dir.join("02-plan-eng.md"), plan_eng).unwrap();
}

#[test]
fn shipped_implement_phase_declares_code_reviewer_subagent() {
    // Sentinel: catch a future commit that strips the tools_required
    // declaration from 03-implement.md without intending to.
    let body = include_str!("../../../teams/dev/phases/03-implement.md");
    let template = PhaseTemplate::parse(body).unwrap();
    assert!(
        template
            .tools_required
            .subagents
            .iter()
            .any(|s| s == "code-reviewer"),
        "implement phase must declare code-reviewer in tools_required.subagents",
    );
    assert!(
        template
            .required_outputs
            .iter()
            .any(|p| p.ends_with("/code-review.md")),
        "implement phase must list code-review.md as a required_output",
    );
}

/// Full M0.5.7 chain:
///   1. set CLAUDE_CONFIG_HOME → tempdir/claude
///   2. stage all 8 plugin agent source files there
///   3. bootstrap_project (ln -sf 8 agents into <claude>/agents/)
///   4. write the *real* shipped implement.md to phases dir
///   5. Orchestrator::new — tool-surface validator must pass because
///      bootstrap_project just registered code-reviewer
#[test]
fn fresh_project_passes_tool_surface_validator_for_shipped_implement() {
    let tmp = TempDir::new().unwrap();
    let claude_dir = tmp.path().join("claude");
    stage_plugin_sources(&claude_dir);

    let _claude_guard = ScopedClaude::set(&claude_dir);

    let paths = CcteamPaths {
        root: tmp.path().join("ccteam-home"),
        projects_root: tmp.path().join("projects"),
    };
    write_global_phases_with_implement(&paths.phases_dir());

    let slug = slugify("review smoke");
    bootstrap_project(&paths, &slug, "review smoke", "dev").unwrap();

    // Confirm bootstrap actually placed the symlink — this is the
    // M0.5.1 + M0.5.3 contract.
    let target = claude_dir.join("agents").join("code-reviewer.md");
    assert!(
        target.exists(),
        "bootstrap_project must ln -sf code-reviewer.md into {}",
        target.display(),
    );
    let meta = std::fs::symlink_metadata(&target).unwrap();
    assert!(meta.file_type().is_symlink());

    // The validator is what would otherwise fail loudly. Construction
    // succeeding with default config (skip_tool_check=false) is the
    // acceptance.
    let orch = Orchestrator::new(paths, OrchestratorConfig::default()).unwrap();
    assert_eq!(orch.templates().len(), 2);
    let names: Vec<&str> = orch.templates().iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"implement"));
    assert!(names.contains(&"plan-eng"));
}

/// SubagentStop appears *after* phase_done in real Claude Code 2.x
/// (subagent shutdown happens after the parent turn's Stop hook
/// fires). Our state machine must still advance — confirmed in
/// `progress::latest_terminal_event_for_phase` unit tests, but worth
/// re-asserting at the orchestrator decision level for M0.5.7.
#[test]
fn implement_phase_advances_when_subagent_done_lands_after_phase_done() {
    let tmp = TempDir::new().unwrap();
    let paths = CcteamPaths {
        root: tmp.path().join("ccteam-home"),
        projects_root: tmp.path().join("projects"),
    };
    let slug = "subagent-after-phase-done";
    let project_dir = paths.project_dir(slug);
    std::fs::create_dir_all(project_dir.join(".ccteam")).unwrap();

    // State: implement is in-flight after a code-reviewer Task launch.
    let mut state = ProjectState::initial(slug.into());
    state.current_phase = "implement".into();
    state.phase_state = PhaseState::InFlight;
    state.save(&paths.project_state(slug)).unwrap();

    // Simulate the production event sequence: phase_inject (impl
    // started), the code-reviewer subagent's PreToolUse / PostToolUse,
    // then parse-phase-end's phase_done, then a trailing SubagentStop
    // for the code-reviewer.
    let progress_path = paths.progress_jsonl(slug);
    let events = [
        json!({"event": "phase_inject", "phase": "implement"}),
        json!({"event": "PreToolUse", "tool": "Task"}),
        json!({"event": "PostToolUse", "tool": "Task"}),
        json!({"event": "Stop"}),
        json!({
            "event": "phase_done",
            "phase": "implement",
            "ts": Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        }),
        // Real Claude Code 2.1.x flushes SubagentStop *after* Stop.
        json!({"event": "SubagentStop", "subagent_type": "code-reviewer"}),
    ];
    for e in &events {
        progress::append_event(&progress_path, e).unwrap();
    }

    let read = progress::read_all_events(&progress_path).unwrap();
    let dag = dev_dag();
    let action = decide_tick_from_events(&dag, &state, &read);
    assert_eq!(
        action,
        TickAction::AdvancePhase {
            from: "implement".into(),
            to: dag.next_on_done("implement").map(String::from),
        },
        "SubagentStop tail must NOT mask phase_done — orchestrator should still advance",
    );
}

/// Negative case: if bootstrap is skipped (no symlink + no fallback),
/// orchestrator startup must reject with a fix hint. This locks in
/// the "fail loud, never silently silent-fail" contract.
#[test]
fn missing_code_reviewer_fails_orchestrator_construction_with_fix_hint() {
    let tmp = TempDir::new().unwrap();
    let claude_dir = tmp.path().join("claude-empty");
    std::fs::create_dir_all(&claude_dir).unwrap();
    // Note: we deliberately do NOT stage plugin sources, so no link
    // bootstrap can produce.

    let _claude_guard = ScopedClaude::set(&claude_dir);

    let paths = CcteamPaths {
        root: tmp.path().join("ccteam-home"),
        projects_root: tmp.path().join("projects"),
    };
    write_global_phases_with_implement(&paths.phases_dir());

    // Bootstrap will warn (source missing) but not fail. The
    // orchestrator validator is what we expect to fail loudly.
    let slug = slugify("missing reviewer");
    bootstrap_project(&paths, &slug, "missing reviewer", "dev").unwrap();

    let err = Orchestrator::new(paths, OrchestratorConfig::default()).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("code-reviewer"),
        "error must name the missing subagent, got: {msg}",
    );
    assert!(
        msg.contains("ccteam doctor"),
        "error must include the fix hint, got: {msg}",
    );
}

/// M2.1: phase YAML's `sub_skills` reference plugin agents that may
/// not be in the hardcoded RECOMMENDED_AGENTS set. The phase-aware
/// linker walks each template's sub_skills and ln -sfs the plugin
/// source so `Task(subagent_type=...)` finds the agent at session
/// start.
#[test]
fn link_phase_subagents_picks_up_sub_skill_plugin_agent() {
    let tmp = TempDir::new().unwrap();
    let claude_dir = tmp.path().join("claude");
    stage_plugin_sources(&claude_dir);
    // Stage an extra plugin agent that isn't in RECOMMENDED_AGENTS, so
    // the test proves the extension does more than the hardcoded
    // recommended set.
    let custom = claude_dir
        .join("plugins/marketplaces/claude-plugins-official/plugins/custom-toolkit/agents/custom-reviewer.md");
    std::fs::create_dir_all(custom.parent().unwrap()).unwrap();
    std::fs::write(&custom, "# custom-reviewer stub\n").unwrap();

    let _guard = ScopedClaude::set(&claude_dir);

    // Phase template with one sub_skill referencing the custom agent.
    let body = concat!(
        "---\n",
        "name: review\n",
        "parallelism: solo\n",
        "sub_skills:\n",
        "  - skill: claude-plugins-official:custom-toolkit/agents/custom-reviewer.md\n",
        "    trigger: phase_done\n",
        "    output_to: .ccteam/custom-review.md\n",
        "---\n",
        "body\n",
    );
    let template = PhaseTemplate::parse(body).unwrap();

    let reports =
        link_recommended_agents_for_phases_into(&claude_dir, &[template], LinkOptions::default())
            .unwrap();

    // Must include the 8 hardcoded agents *and* the new one.
    assert_eq!(
        reports.len(),
        RECOMMENDED_AGENTS.len() + 1,
        "expected {} reports, got {}",
        RECOMMENDED_AGENTS.len() + 1,
        reports.len(),
    );
    let custom_report = reports
        .iter()
        .find(|r| r.agent.filename == "custom-reviewer.md")
        .expect("custom-reviewer plugin agent must appear in reports");
    assert_eq!(custom_report.action, AgentLinkAction::Linked);
    assert!(claude_dir
        .join("agents/custom-reviewer.md")
        .symlink_metadata()
        .unwrap()
        .file_type()
        .is_symlink());
}

/// Sub-skill referencing a hook script (`.sh`) instead of an agent
/// must NOT be auto-linked into `~/.claude/agents/` — only `.md`
/// files under `<plugin>/agents/` count as Task-callable subagents.
#[test]
fn link_phase_subagents_skips_non_agent_sub_skill_paths() {
    let tmp = TempDir::new().unwrap();
    let claude_dir = tmp.path().join("claude");
    stage_plugin_sources(&claude_dir);

    let _guard = ScopedClaude::set(&claude_dir);

    let body = concat!(
        "---\n",
        "name: ship\n",
        "parallelism: solo\n",
        "sub_skills:\n",
        "  - skill: claude-plugins-official:security-guidance/hooks/security_reminder_hook.py\n",
        "    trigger: phase_start\n",
        "    output_to: .ccteam/precheck.md\n",
        "---\n",
    );
    let template = PhaseTemplate::parse(body).unwrap();

    let reports =
        link_recommended_agents_for_phases_into(&claude_dir, &[template], LinkOptions::default())
            .unwrap();
    // Just the 8 hardcoded — the .py hook is not eligible for Task
    // dispatch, so no extra link.
    assert_eq!(reports.len(), RECOMMENDED_AGENTS.len());
}

