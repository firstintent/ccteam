//! ccteam-core: orchestrator state machine, protocol types, tmux wrapper,
//! and hook-shared schemas. Consumed by `ccteam-cli` (binary entry) and
//! `ccteam-hooks` (hook handlers invoked via `ccteam hook ...`).

pub mod orchestrator;
pub mod paths;
pub mod phases;
pub mod progress;
pub mod state;
pub mod templates;
pub mod tmux;

pub use orchestrator::{
    decide_tick, is_terminal, next_phase, Orchestrator, OrchestratorConfig, TickAction,
    FIRST_PHASE, M0_PHASE_DAG,
};
pub use paths::{slug_from_project_dir, CcteamPaths};
pub use phases::{
    AgentTeamRole, PhaseHooks, PhaseTemplate, SubSkillSpec, SubSkillTrigger,
};
pub use state::{Parallelism, PhaseHistoryEntry, PhaseState, ProjectState};
pub use templates::{write_project_settings, PROJECT_SETTINGS_JSON};
pub use tmux::{pid_is_alive, session_name_for_slug, tmux_available, TmuxSession};

/// Crate version, identical to the workspace package version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
