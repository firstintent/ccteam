//! ccteam-core: orchestrator state machine, protocol types, tmux wrapper,
//! and hook-shared schemas. Consumed by `ccteam-cli` (binary entry) and
//! `ccteam-hooks` (hook handlers invoked via `ccteam hook ...`).

pub mod cost;
pub mod fix_loop;
pub mod orchestrator;
pub mod paths;
pub mod phases;
pub mod progress;
pub mod projects;
pub mod stall;
pub mod state;
pub mod templates;
pub mod tmux;
pub mod tool_surface;

pub use fix_loop::{FixLoopDecision, FixLoopFrontMatter, FixLoopState};
pub use orchestrator::{
    append_progress_summary, build_progress_summary, decide_tick, decide_tick_from_events,
    is_terminal, next_phase, Orchestrator, OrchestratorConfig, TickAction, FIRST_PHASE,
    M0_PHASE_DAG,
};
pub use projects::{bootstrap_project, pick_unused_slug, pre_trust_project, slugify};
pub use cost::{classify as classify_cost, CostLevel, COST_MID_WARN_USD};
pub use stall::{
    classify as classify_stall, silent_seconds, StallLevel, STALL_ESCALATE_SECONDS,
    STALL_SUSPICIOUS_SECONDS, STALL_WARN_SECONDS,
};
pub use paths::{slug_from_project_dir, CcteamPaths};
pub use phases::{
    AgentTeamRole, PhaseHooks, PhaseTemplate, SubSkillSpec, SubSkillTrigger,
};
pub use state::{Parallelism, PhaseHistoryEntry, PhaseState, ProjectState};
pub use templates::{
    current_ccteam_bin, project_phase_filename, render_project_settings,
    write_global_phase_templates, write_project_phase_templates, write_project_settings,
    SettingsEnv, PHASE_TEMPLATES, PROJECT_SETTINGS_JSON,
};
pub use tmux::{pid_is_alive, session_name_for_slug, tmux_available, TmuxSession};
pub use tool_surface::{
    ensure_skills_placeholders, link_recommended_agents, link_recommended_agents_into,
    user_claude_dir, AgentLinkAction, AgentLinkReport, LinkOptions, RecommendedAgent,
    RECOMMENDED_AGENTS,
};

/// Crate version, identical to the workspace package version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
