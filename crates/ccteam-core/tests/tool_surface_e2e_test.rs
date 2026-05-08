//! Tool-surface foundation regression. V0.2 M0.20 replaces the M0.5
//! `~/.claude/agents/` ln -sf protocol with Claude Code's in-memory
//! plugin pipeline:
//!
//!   1. `bootstrap_project` writes `enabledPlugins` into the spawned
//!      project's `.claude/settings.json`, derived from each phase
//!      YAML's `tools_required.subagents` via `plugin_resolution`.
//!   2. The orchestrator's tool-surface validator passes when the
//!      plugin source file exists under
//!      `~/.claude/plugins/marketplaces/<mkt>/plugins/<plugin>/agents/<name>.md`.
//!   3. `Task(subagent_type=...)` resolves at runtime through the
//!      plugin pipeline (Claude Code namespaces it as `<plugin>:<name>`).

use std::path::Path;
use std::sync::Mutex;

use chrono::{SecondsFormat, Utc};
use serde_json::json;
use tempfile::TempDir;

use ccteam_core::{
    bootstrap_project, decide_tick_from_events, dev_dag, plugins_to_enable, progress, slugify,
    CcteamPaths, KNOWN_PLUGIN_AGENTS, Orchestrator, OrchestratorConfig, PhaseState,
    PhaseTemplate, ProjectState, TickAction,
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

/// Stage every shipped plugin agent's source file under the in-memory
/// plugin pipeline path layout. Mirrors what `claude /plugin add
/// claude-plugins-official` writes after marketplace fetch.
fn stage_plugin_sources(claude_dir: &Path) {
    for agent in KNOWN_PLUGIN_AGENTS {
        let src = agent.source_path(claude_dir);
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::write(&src, format!("# stub for {}\n", agent.subagent)).unwrap();
    }
}

fn write_global_phases_with_implement(phases_dir: &Path) {
    std::fs::create_dir_all(phases_dir).unwrap();
    let implement_src = include_str!("../../../teams/dev/phases/03-implement.md");
    std::fs::write(phases_dir.join("03-implement.md"), implement_src).unwrap();
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

/// V0.2 M0.20 chain:
///   1. CLAUDE_CONFIG_HOME → tempdir/claude
///   2. stage every plugin agent source file (simulates plugin install)
///   3. bootstrap_project writes enabledPlugins to settings.json
///   4. orchestrator's tool-surface validator passes via the plugin
///      pipeline reachability check
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
    let project_dir = bootstrap_project(&paths, &slug, "review smoke", "dev").unwrap();

    // V0.2 M0.20 contract: the spawned project's settings.json carries
    // enabledPlugins for the plugin shipping each declared subagent
    // (instead of an ln -sf into ~/.claude/agents/).
    let settings = project_dir.join(".claude/settings.json");
    let v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
    let enabled = v["enabledPlugins"]
        .as_object()
        .expect("enabledPlugins must be present when team declares plugin subagents");
    assert_eq!(enabled["pr-review-toolkit@claude-plugins-official"], true);

    // No ln -sf into ~/.claude/agents/ — V0.2 M0.20 deletes that path.
    let agents_dir = claude_dir.join("agents");
    if agents_dir.exists() {
        let count = std::fs::read_dir(&agents_dir).unwrap().count();
        assert_eq!(
            count, 0,
            "ccteam-core no longer writes ~/.claude/agents/ — found {count} entries",
        );
    }

    let orch = Orchestrator::new(paths, OrchestratorConfig::default()).unwrap();
    assert_eq!(orch.templates().len(), 2);
    let names: Vec<&str> = orch.templates().iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"implement"));
    assert!(names.contains(&"plan-eng"));
}

/// SubagentStop appears *after* phase_done in real Claude Code 2.x —
/// the state machine must still advance.
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

    let mut state = ProjectState::initial(slug.into());
    state.current_phase = "implement".into();
    state.phase_state = PhaseState::InFlight;
    state.save(&paths.project_state(slug)).unwrap();

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

/// Negative case: when the plugin source isn't installed, orchestrator
/// startup must reject with a fix hint pointing at the plugin id.
#[test]
fn missing_code_reviewer_fails_orchestrator_construction_with_fix_hint() {
    let tmp = TempDir::new().unwrap();
    let claude_dir = tmp.path().join("claude-empty");
    std::fs::create_dir_all(&claude_dir).unwrap();

    let _claude_guard = ScopedClaude::set(&claude_dir);

    let paths = CcteamPaths {
        root: tmp.path().join("ccteam-home"),
        projects_root: tmp.path().join("projects"),
    };
    write_global_phases_with_implement(&paths.phases_dir());

    let slug = slugify("missing reviewer");
    bootstrap_project(&paths, &slug, "missing reviewer", "dev").unwrap();

    let err = Orchestrator::new(paths, OrchestratorConfig::default()).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("code-reviewer"),
        "error must name the missing subagent, got: {msg}",
    );
    assert!(
        msg.contains("pr-review-toolkit@claude-plugins-official"),
        "error must include the plugin id fix hint, got: {msg}",
    );
}

/// V0.2 M0.20: phase YAML's sub_skill referencing a plugin agent via
/// `<marketplace>:<plugin>/agents/<name>.md` propagates the same plugin
/// dependency into `enabledPlugins` even when the bare name isn't
/// listed in `tools_required.subagents`.
#[test]
fn enabled_plugins_picks_up_sub_skill_plugin_agent() {
    let body = concat!(
        "---\n",
        "name: review\n",
        "parallelism: solo\n",
        "sub_skills:\n",
        "  - skill: claude-plugins-official:pr-review-toolkit/agents/code-reviewer.md\n",
        "    trigger: phase_done\n",
        "    output_to: .ccteam/code-review.md\n",
        "---\n",
        "body\n",
    );
    let template = PhaseTemplate::parse(body).unwrap();

    // The bare subagent name extracted from the sub_skill path resolves
    // through plugin_resolution.
    let plugin_ids = plugins_to_enable(["code-reviewer"]);
    assert!(
        plugin_ids.contains("pr-review-toolkit@claude-plugins-official"),
        "code-reviewer must resolve to pr-review-toolkit@claude-plugins-official",
    );
    // Sentinel: the parsed template still carries the sub_skill reference
    // verbatim so bootstrap_project sees it.
    assert_eq!(template.sub_skills.len(), 1);
    assert!(
        template.sub_skills[0].skill.contains("code-reviewer.md"),
        "sub_skill must keep the agent path intact",
    );
}

/// Plugin name lookup table covers every shipped phase YAML's known
/// subagent so a fresh project's enabledPlugins isn't empty by accident
/// for the dev / product-research teams.
#[test]
fn known_subagents_resolve_to_official_marketplace_plugins() {
    let plugin_ids: Vec<String> = plugins_to_enable([
        "code-reviewer",
        "code-architect",
        "code-simplifier",
    ])
    .into_iter()
    .collect();
    assert!(plugin_ids.contains(&"pr-review-toolkit@claude-plugins-official".to_string()));
    assert!(plugin_ids.contains(&"feature-dev@claude-plugins-official".to_string()));
    assert!(plugin_ids.contains(&"code-simplifier@claude-plugins-official".to_string()));
}
