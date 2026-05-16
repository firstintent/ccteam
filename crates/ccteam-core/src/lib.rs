//! ccteam-core: orchestrator state machine, protocol types, tmux wrapper,
//! and hook-shared schemas. Consumed by `ccteam-cli` (binary entry) and
//! `ccteam-hooks` (hook handlers invoked via `ccteam hook ...`).
//!
//! V0.4.0 F60: the phase machinery modules (`phases`, `golden_rules`,
//! `dag`, `subskill`) and the team-template loaders have been deleted.
//! The thin `Orchestrator` stub stays so `ccteam start` and tests keep
//! compiling; F66 rebuilds dispatch against the new `workflow.yaml`
//! shape (F63) and artifact-trigger watcher (F64).

pub mod actions;
// V0.4.0 F64 — artifact-trigger filesystem watcher (inotify / fsevents).
// Emits ArtifactEvent for every workflow.yaml `Trigger::Watch(<path>)`
// agent. See module docs + docs/v0-4-0/prd.md §6.2.
pub mod artifact_watcher;
pub mod auto_loop;
// V0.4.5 F80 — Liveness probe for `claude --bg` background jobs.
// Cross-references the recorded `job_id` against
// `~/.claude/jobs/<id>/state.json` so consumers (web UI, orchestrator
// poll loop) can distinguish "really running" from "stale agent_spawn
// after daemon SIGKILL". See module docs.
pub mod claude_job;
// V0.4.2 F73 — `~/.ccteam/config.yaml` global config + projects registry.
pub mod config;
pub mod cost;
pub mod daemon;
pub mod harness;
pub mod inbox;
pub mod memory_bridge;
pub mod meta_agent;
// V0.4.2 F74 — one-shot migration (V0.4.1 → V0.4.2 config.yaml fold).
pub mod migration;
pub mod orchestrator;
pub mod paths;
pub mod pending_inject;
pub mod plugin_resolution;
pub mod progress;
pub mod projects;
pub mod queries;
pub mod screenshot;
pub mod silence_classifier;
pub mod skill;
pub mod stall;
pub mod state;
pub mod team;
pub mod team_factory;
pub mod team_resolver;
pub mod templates;
pub mod tmux;
pub mod tool_surface;
pub mod watchdog;
// V0.4.0 F63 — workflow.yaml schema + parser. Pure data + validation;
// no IO side effects beyond reading the YAML file. See module docs.
pub mod workflow;

pub use actions::{
    inject_decision, next_inbox_seq, pause, resume, send_to_session, send_to_session_with,
    DecisionInput, SendOptions, SendResult,
};
pub use auto_loop::{AutoLoopDecision, AutoLoopFrontMatter, AutoLoopState};
pub use claude_job::{classify as classify_job_state, probe_job, probe_state_json, JobLiveness};
pub use config::{
    append_project as append_project_to_config, config_path as ccteam_config_path,
    load as load_ccteam_config, lookup_project as lookup_project_in_config,
    remove_project as remove_project_from_config, save as save_ccteam_config,
    upsert_project as upsert_project_in_config, CcteamConfig, ProjectEntry, CONFIG_FILENAME,
};
pub use cost::{classify as classify_cost, CostLevel, COST_MID_WARN_USD};
pub use daemon::{
    check_health as check_daemon_health, check_health_at as check_daemon_health_at, heartbeat_path,
    pidfile_path, read_pidfile, remove_heartbeat, remove_pidfile, send_sigterm_to_pidfile,
    write_heartbeat, write_pidfile, DaemonHealth, HEARTBEAT_GRACE, HEARTBEAT_INTERVAL,
    HEARTBEAT_NAME, PIDFILE_NAME,
};
pub use harness::{
    parse_cc_state_json, state_json_path, ClaudeCodeAdapter, CodexAdapter, HarnessAdapter,
    HarnessError, HarnessSnapshot, SessionHandle, SpawnOpts, SubagentState, CLAUDE_BIN_ENV,
    CLAUDE_JOBS_DIR_ENV, CODEX_STATUS_MARKER, CODEX_STATUS_TAIL_LINES, DEFAULT_CLAUDE_SID,
};
pub use inbox::{
    inbox_filename, outbox_filename, InboxAttachment, InboxFrontMatter, InboxMessage,
    OutboxEventKind, OutboxFrontMatter, OutboxMessage, OutboxPriority, SessionMailbox,
    LATEST_SCHEMA_VERSION,
};
pub use memory_bridge::{
    install_into as install_memory_bridge_into, install_memory_bridge, InstallMemoryBridgeOptions,
    MemoryBridgeAction, MemoryBridgeReport,
};
pub use meta_agent::{
    bootstrap_meta_project, clean_stale_meta_layouts, meta_session_name, meta_slug,
    render_meta_role_prompt, MetaBootstrapReport, META_SESSION_NAME, META_SLUG, META_TEAM_NAME,
};
pub use migration::{
    migrate_v041_to_v042, render_migration_report, MigrationReport as V042MigrationReport,
};
pub use orchestrator::MAX_CONCURRENT_PROJECTS;
pub use orchestrator::{Orchestrator, OrchestratorConfig, DEFAULT_CLAUDE_MODEL};
pub use paths::{
    session_context_from_cwd, slug_from_project_dir, CcteamPaths, ProjectSessionContext,
};
pub use pending_inject::{
    delete as delete_pending_inject, load as load_pending_inject, pending_inject_path_in,
    save as save_pending_inject, PendingInject, DEFAULT_MAX_DEFER_MINUTES, PENDING_INJECT_FILE,
};
pub use plugin_resolution::{
    lookup_plugin_agent, plugins_to_enable, PluginAgent, KNOWN_PLUGIN_AGENTS,
};
pub use progress::{
    current_agent_sessions, escalation_count, workflow_cost_total, AgentSessionStatus,
    AgentSessionSummary,
};
pub use projects::{
    bootstrap_project, bootstrap_project_at_dir, pick_unused_slug, pick_unused_slug_verbatim,
    pre_trust_project, slugify, slugify_brief, validate_slug_format,
};
pub use queries::{
    active_sessions, artifact_queue, collect_projects, collect_recent_events, cost_history_buckets,
    cost_summary, job_log_tail, workflow_summary, ActiveSessionInfo, AgentStatus,
    ArtifactQueueEntry, CostHistoryBucket, CostSummary, ProjectSummary, WorkflowSummary,
};
pub use screenshot::{
    probe_font as probe_screenshot_font, render_screenshot, vt100_color_to_rgb, ScreenshotResult,
    FONT_ENV as SCREENSHOT_FONT_ENV,
};
pub use silence_classifier::{
    classify as classify_silence, load_retry_count as load_limbo_retry_count,
    reset_retry_count as reset_limbo_retry_count, retry_path_in as limbo_retry_path_in,
    save_retry_count as save_limbo_retry_count, LastEventSummary, LimboAction, LimboRetryCount,
    SilenceClass, LIMBO_RETRY_FILE, MAX_LIMBO_RETRY,
};
pub use skill::{
    install_ccteam_control_skill, install_ccteam_project_creator_skill,
    install_ccteam_team_author_skill, install_into as install_skill_into, install_skill_body_into,
    InstallSkillOptions, InstallSkillReport, SkillInstallAction, CCTEAM_CONTROL_SKILL_NAME,
    CCTEAM_PROJECT_CREATOR_SKILL_NAME, CCTEAM_TEAM_AUTHOR_SKILL_NAME, LEGACY_SKILL_NAMES,
};
pub use stall::{
    classify as classify_stall, classify_with_thresholds, silent_seconds, StallLevel,
    StallThresholds, STALL_ESCALATE_SECONDS, STALL_SUSPICIOUS_SECONDS, STALL_WARN_SECONDS,
};
pub use state::{
    harness_sid_prefix, Parallelism, PhaseHistoryEntry, PhaseState, ProjectState, SessionRecord,
};
pub use team::{
    CostPolicy, CriticDimensionSpec, CriticStrictness, DefaultSessionSpec, DomainRule,
    EscalateGrammarExtension, EscalateRoute, GoldenRuleEnforcement, HarnessKind, ProtocolRule,
    RetroFieldKind, RetroFieldSpec, TeamGoldenRules, TeamKind, TeamSpec,
};
pub use team_factory::{
    init_team_staging, publish_team, staging_dir_for, validate_staged_team, InitReport,
    PhaseScaffold, PluginAuthor, PluginManifest, PublishInput, PublishReport, PublishTarget,
    TeamInitInput,
};
pub use team_resolver::{
    default_user_staging_dir, discover_team_names, resolve_team, save_team, TeamResolveContext,
    TeamSource, TEAM_SOURCES,
};
pub use templates::{
    current_ccteam_bin, render_project_settings, write_global_helper_templates,
    write_project_settings, EnabledPluginsSetting, SettingsEnv, HELPER_TEMPLATES,
    PROJECT_SETTINGS_JSON,
};
pub use tmux::{
    capture_pane_tail, capture_pane_tail_from_session, capture_pane_with_ansi,
    capture_pane_with_ansi_from_session, pid_is_alive, query_pane_dims,
    query_pane_dims_from_session, session_name_for_project, session_name_for_slug, tmux_available,
    TmuxSession,
};
pub use tool_surface::{
    disable_tool_surface_bootstrap_for_tests, ensure_skills_placeholders,
    migrate_legacy_skill_dirs, migrate_recommended_agent_symlinks, missing_tools,
    rewrite_legacy_hook_commands, user_claude_dir, HookCmdRewriteAction, HookCmdRewriteReport,
    LegacySkillAction, LegacySkillReport, MigrationReport, MissingTool, ToolSurfaceSnapshot,
    ToolsRequired, BUILTIN_SUBAGENTS,
};
pub use watchdog::{
    config_path as watchdog_config_path, load_config as load_watchdog_config,
    push_alert_to_meta_outbox as push_watchdog_alert_to_meta_outbox, scan as watchdog_scan,
    AlertKind as WatchdogAlertKind, NotifyMode as WatchdogNotifyMode, WatchdogAlert,
    WatchdogConfig, DEFAULT_NOTIFY_ON_CYCLE_COUNT, WATCHDOG_CONFIG_FILENAME,
};
// V0.4.0 F63 — workflow.yaml schema.
pub use workflow::{AgentSpec, Executor, OnTimeout, Trigger, WorkflowError, WorkflowSpec};
// V0.4.0 F64 — artifact watcher event types + watcher entry point.
pub use artifact_watcher::{ArtifactEvent, ArtifactWatcher, WatchKind, DEBOUNCE_WINDOW};

/// Crate version, identical to the workspace package version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
