//! ccteam-flow: workflow orchestration engine and filesystem watchers.
//!
//! This crate owns the runtime flow layer that compiles workflow specs
//! into long-running daemon behavior. Stable data primitives remain in
//! `ccteam-core`; process execution primitives live in `ccteam-harness`.

pub mod artifact_watcher;
pub mod orchestrator;
pub mod workflow;
pub mod workflow_watcher;

pub use artifact_watcher::{
    AgentTeamsWatcher, AgentTeamsWatcherConfig, ArtifactEvent, ArtifactWatcher, WatchKind,
    DEBOUNCE_WINDOW, TEAMS_DISCOVERY_INTERVAL,
};
pub use orchestrator::{
    CancelReason, Orchestrator, OrchestratorConfig, TeamEvent, DEFAULT_CLAUDE_MODEL,
    MAX_CONCURRENT_PROJECTS,
};
pub use workflow::{
    AgentSpec, AgentTeamSpec, BudgetSpec, CleanupOnStop, Executor, OnTimeout,
    PlanApprovalOnTimeout, PlanApprovalSpec, SuggestedTeammate, SuggestedTeammateKind, Trigger,
    WorkflowError, WorkflowMode, WorkflowSpec,
};
pub use workflow_watcher::{
    WorkflowFileEvent, WorkflowFileEventKind, WorkflowFileWatcher,
    DEBOUNCE_WINDOW as WORKFLOW_WATCHER_DEBOUNCE_WINDOW,
};
