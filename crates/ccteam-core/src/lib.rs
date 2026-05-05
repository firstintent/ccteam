//! ccteam-core: orchestrator state machine, protocol types, tmux wrapper,
//! and hook-shared schemas. Consumed by `ccteam-cli` (binary entry) and
//! `ccteam-hooks` (hook handlers invoked via `ccteam hook ...`).

pub mod cost;
pub mod daemon;
pub mod dag;
pub mod fix_loop;
pub mod inbox;
pub mod meta_agent;
pub mod orchestrator;
pub mod skill;
pub mod paths;
pub mod phases;
pub mod progress;
pub mod projects;
pub mod stall;
pub mod state;
pub mod team;
pub mod templates;
pub mod tmux;
pub mod tool_surface;

pub use dag::{dev_dag, Dag};
pub use fix_loop::{FixLoopDecision, FixLoopFrontMatter, FixLoopState};
pub use inbox::{
    inbox_filename, outbox_filename, InboxAttachment, InboxFrontMatter, InboxMessage,
    OutboxEventKind, OutboxFrontMatter, OutboxMessage, OutboxPriority, SessionMailbox,
    LATEST_SCHEMA_VERSION,
};
pub use meta_agent::{
    bootstrap_meta_project, meta_session_name, meta_slug, render_meta_role_prompt,
    MetaBootstrapReport, META_SESSION_PREFIX, META_TEAM_NAME,
};
pub use skill::{
    install_ccteam_control_skill, install_into as install_skill_into,
    InstallSkillOptions, InstallSkillReport, SkillInstallAction,
    CCTEAM_CONTROL_SKILL_NAME,
};
pub use orchestrator::MAX_CONCURRENT_PROJECTS;
pub use orchestrator::{
    append_progress_summary, build_progress_summary, check_phase_tools, decide_tick,
    decide_tick_from_events, Orchestrator, OrchestratorConfig, TickAction,
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
    AgentTeamRole, PhaseHooks, PhaseTemplate, SubSkillSpec, SubSkillTrigger,
};
pub use state::{Parallelism, PhaseHistoryEntry, PhaseState, ProjectState};
pub use team::{RetroFieldKind, RetroFieldSpec, TeamSpec};
pub use templates::{
    current_ccteam_bin, project_phase_filename, render_project_settings,
    write_global_helper_templates, write_global_phase_templates,
    write_project_phase_templates, write_project_settings, SettingsEnv, HELPER_TEMPLATES,
    PHASE_TEMPLATES, PROJECT_SETTINGS_JSON,
};
pub use tmux::{pid_is_alive, session_name_for_slug, tmux_available, TmuxSession};
pub use tool_surface::{
    disable_tool_surface_bootstrap_for_tests, ensure_skills_placeholders,
    link_recommended_agents, link_recommended_agents_into, missing_tools, user_claude_dir,
    AgentLinkAction, AgentLinkReport, LinkOptions, MissingTool, RecommendedAgent,
    ToolSurfaceSnapshot, ToolsRequired, BUILTIN_SUBAGENTS, RECOMMENDED_AGENTS,
};

/// Crate version, identical to the workspace package version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
