//! ccteam-core: orchestrator state machine, protocol types, tmux wrapper,
//! and hook-shared schemas. Consumed by `ccteam-cli` (binary entry) and
//! `ccteam-hooks` (hook handlers invoked via `ccteam hook ...`).

pub mod cost;
pub mod daemon;
pub mod dag;
pub mod auto_loop;
pub mod golden_rules;
pub mod inbox;
pub mod meta_agent;
pub mod orchestrator;
pub mod memory_bridge;
pub mod skill;
pub mod paths;
pub mod phases;
pub mod progress;
pub mod projects;
pub mod stall;
pub mod state;
pub mod subskill;
pub mod team;
pub mod templates;
pub mod tmux;
pub mod tool_surface;

pub use dag::{dev_dag, Dag};
pub use auto_loop::{AutoLoopDecision, AutoLoopFrontMatter, AutoLoopState};
pub use golden_rules::{
    enforce as enforce_golden_rules, GoldenRuleKindLabel, GoldenRuleSkipped,
    GoldenRuleViolation, GoldenRulesReport,
};
pub use inbox::{
    inbox_filename, outbox_filename, InboxAttachment, InboxFrontMatter, InboxMessage,
    OutboxEventKind, OutboxFrontMatter, OutboxMessage, OutboxPriority, SessionMailbox,
    LATEST_SCHEMA_VERSION,
};
pub use meta_agent::{
    bootstrap_meta_project, meta_session_name, meta_slug, render_meta_role_prompt,
    MetaBootstrapReport, META_SESSION_PREFIX, META_TEAM_NAME,
};
pub use memory_bridge::{
    install_into as install_memory_bridge_into, install_memory_bridge,
    InstallMemoryBridgeOptions, MemoryBridgeAction, MemoryBridgeReport,
};
pub use skill::{
    install_ccteam_control_skill, install_into as install_skill_into,
    InstallSkillOptions, InstallSkillReport, SkillInstallAction,
    CCTEAM_CONTROL_SKILL_NAME,
};
pub use orchestrator::MAX_CONCURRENT_PROJECTS;
pub use orchestrator::{
    append_progress_summary, build_progress_summary, check_phase_tools, decide_tick,
    decide_tick_from_events, intersect_open_decisions_with_required_inputs, Orchestrator,
    OrchestratorConfig, TeamRuntime, TickAction,
};
pub use projects::{bootstrap_project, pick_unused_slug, pre_trust_project, slugify};
pub use cost::{classify as classify_cost, CostLevel, COST_MID_WARN_USD};
pub use daemon::{
    pidfile_path, read_pidfile, remove_pidfile, send_sigterm_to_pidfile, write_pidfile,
    PIDFILE_NAME,
};
pub use stall::{
    classify as classify_stall, classify_with_thresholds, silent_seconds, StallLevel,
    StallThresholds, STALL_ESCALATE_SECONDS, STALL_SUSPICIOUS_SECONDS, STALL_WARN_SECONDS,
};
pub use paths::{slug_from_project_dir, CcteamPaths};
pub use phases::{
    AgentTeamRole, DecisionMode, GoldenRule, GoldenRuleKind, PhaseHooks, PhaseTemplate,
    SubSkillSpec, SubSkillTrigger,
};
pub use subskill::{
    resolve_skill_path, run_sub_skills_for_phase, ClaudePRunner, SubSkillOutcome, SubSkillRunCtx,
    SubSkillRunner,
};
pub use state::{Parallelism, PhaseHistoryEntry, PhaseState, ProjectState};
pub use team::{
    CostPolicy, CriticDimensionSpec, CriticStrictness, EscalateGrammarExtension, EscalateRoute,
    RetroFieldKind, RetroFieldSpec, TeamSpec,
};
pub use templates::{
    current_ccteam_bin, project_phase_filename, render_project_settings,
    write_all_global_team_templates, write_global_helper_templates,
    write_global_phase_templates, write_project_phase_templates,
    write_project_phase_templates_for_team, write_project_settings, SettingsEnv,
    HELPER_TEMPLATES, PHASE_TEMPLATES, PROJECT_SETTINGS_JSON,
};
// V0.2 §6.4 candidate 3: TEAM_BUNDLES / team_bundle / TeamTemplateBundle
// are no longer exported. They remain `pub(crate)` as the in-binary
// seed source for `write_all_global_team_templates`; runtime code paths
// outside ccteam-core query disk (`<global_dir>/teams/<name>/team.yaml`)
// instead.
pub use tmux::{pid_is_alive, session_name_for_slug, tmux_available, TmuxSession};
pub use tool_surface::{
    disable_tool_surface_bootstrap_for_tests, ensure_skills_placeholders,
    link_recommended_agents, link_recommended_agents_for_phases,
    link_recommended_agents_for_phases_into, link_recommended_agents_into, missing_tools,
    user_claude_dir, AgentLinkAction, AgentLinkReport, LinkOptions, MissingTool,
    RecommendedAgent, ToolSurfaceSnapshot, ToolsRequired, BUILTIN_SUBAGENTS,
    RECOMMENDED_AGENTS,
};

/// Crate version, identical to the workspace package version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
