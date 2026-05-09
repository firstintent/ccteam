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
pub mod plugin_resolution;
pub mod progress;
pub mod projects;
pub mod screenshot;
pub mod stall;
pub mod state;
pub mod subskill;
pub mod team;
pub mod team_factory;
pub mod team_resolver;
pub mod templates;
pub mod tmux;
pub mod tool_surface;
pub mod watchdog;

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
    install_cct_control_skill, install_cct_project_creator_skill,
    install_cct_team_author_skill, install_into as install_skill_into,
    install_skill_body_into, InstallSkillOptions, InstallSkillReport, SkillInstallAction,
    CCT_CONTROL_SKILL_NAME, CCT_PROJECT_CREATOR_SKILL_NAME, CCT_TEAM_AUTHOR_SKILL_NAME,
    LEGACY_SKILL_NAMES,
};
pub use orchestrator::MAX_CONCURRENT_PROJECTS;
pub use orchestrator::{
    append_progress_summary, build_progress_summary, check_phase_tools, decide_tick,
    decide_tick_from_events, intersect_open_decisions_with_required_inputs, Orchestrator,
    OrchestratorConfig, TeamRuntime, TickAction, DEFAULT_CLAUDE_MODEL,
};
pub use projects::{
    bootstrap_project, pick_unused_slug, pick_unused_slug_verbatim, pre_trust_project,
    slugify, slugify_brief,
};
pub use cost::{classify as classify_cost, CostLevel, COST_MID_WARN_USD};
pub use daemon::{
    check_health as check_daemon_health, check_health_at as check_daemon_health_at,
    heartbeat_path, pidfile_path, read_pidfile, remove_heartbeat, remove_pidfile,
    send_sigterm_to_pidfile, write_heartbeat, write_pidfile, DaemonHealth, HEARTBEAT_GRACE,
    HEARTBEAT_INTERVAL, HEARTBEAT_NAME, PIDFILE_NAME,
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
    CostPolicy, CriticDimensionSpec, CriticStrictness, DomainRule, EscalateGrammarExtension,
    EscalateRoute, GoldenRuleEnforcement, ProtocolRule, RetroFieldKind, RetroFieldSpec,
    TeamGoldenRules, TeamSpec,
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
    current_cct_bin, project_phase_filename, render_project_settings,
    write_all_global_team_templates, write_global_helper_templates,
    write_global_phase_templates, write_project_phase_templates,
    write_project_phase_templates_for_team, write_project_settings,
    EnabledPluginsSetting, SettingsEnv, HELPER_TEMPLATES, PHASE_TEMPLATES,
    PROJECT_SETTINGS_JSON,
};
// V0.2 §6.4 candidate 3: TEAM_BUNDLES / team_bundle / TeamTemplateBundle
// are no longer exported. They remain `pub(crate)` as the in-binary
// seed source for `write_all_global_team_templates`; runtime code paths
// outside ccteam-core query disk (`<global_dir>/teams/<name>/team.yaml`)
// instead.
pub use screenshot::{
    probe_font as probe_screenshot_font, render_screenshot, vt100_color_to_rgb,
    FONT_ENV as SCREENSHOT_FONT_ENV, ScreenshotResult,
};
pub use tmux::{
    capture_pane_with_ansi, pid_is_alive, query_pane_dims, session_name_for_slug,
    tmux_available, TmuxSession,
};
pub use plugin_resolution::{
    lookup_plugin_agent, plugins_to_enable, PluginAgent, KNOWN_PLUGIN_AGENTS,
};
pub use tool_surface::{
    disable_tool_surface_bootstrap_for_tests, ensure_skills_placeholders,
    migrate_legacy_skill_dirs, migrate_recommended_agent_symlinks, missing_tools,
    rewrite_legacy_hook_commands, user_claude_dir, HookCmdRewriteAction,
    HookCmdRewriteReport, LegacySkillAction, LegacySkillReport, MigrationReport,
    MissingTool, ToolSurfaceSnapshot, ToolsRequired, BUILTIN_SUBAGENTS,
};
pub use watchdog::{
    config_path as watchdog_config_path, load_config as load_watchdog_config,
    push_alert_to_meta_outbox as push_watchdog_alert_to_meta_outbox, scan as watchdog_scan,
    AlertKind as WatchdogAlertKind, NotifyMode as WatchdogNotifyMode, WatchdogAlert,
    WatchdogConfig, DEFAULT_NOTIFY_ON_CYCLE_COUNT, WATCHDOG_CONFIG_FILENAME,
};

/// Crate version, identical to the workspace package version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
