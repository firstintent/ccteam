//! ccteam-core: orchestrator state machine, protocol types, tmux wrapper,
//! and hook-shared schemas. Consumed by `ccteam-cli` (binary entry) and
//! `ccteam-hooks` (hook handlers invoked via `ccteam hook ...`).

pub mod phases;
pub mod state;

pub use phases::{
    AgentTeamRole, PhaseHooks, PhaseTemplate, SubSkillSpec, SubSkillTrigger,
};
pub use state::{Parallelism, PhaseHistoryEntry, PhaseState, ProjectState};

/// Crate version, identical to the workspace package version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
