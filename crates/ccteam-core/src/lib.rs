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
// V0.6.1 F128 — file-mutation helpers for `/ccteam-control
// change-persona` + `add-tool` MCP admin tools. Pure IO over a chat
// bot's `.claude/agents/<bot>.md` definition file.
pub mod admin_actions;
// V0.6.5 F152 + F153 — `mcp__ccteam__advise_vote` /
// `mcp__ccteam__advise_parallel` real implementations. Spawns Claude +
// Codex one-shot advisors in parallel, optional verdict synthesis,
// per-vendor budget ledger under `<ccteam_root>/cost-budget.json`.
pub mod advise;
// V0.6.0 Wave 2 F114 — scientist nickname pool used by ccteam-creator
// when minting bot handles for new chat workflows.
pub mod agent_naming;
// V0.4.0 F64 — artifact-trigger filesystem watcher (inotify / fsevents).
// Emits ArtifactEvent for every workflow.yaml `Trigger::Watch(<path>)`
// agent. See module docs + docs/versions/v0-4-0/prd.md §6.2.
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
// V0.6.3 F142 — `trigger: schedule` cron evaluation (5-field, skip-missed).
pub mod cron;
pub mod daemon;
pub mod defaults;
// V0.6.0 F107 — adapter implementations behind the HarnessAdapter trait
// now live in ccteam-harness; concrete execution adapters move next.
pub mod execution;
// V0.6.0 F115 — agent handoff doc mechanism (`.ccteam/handoffs/`).
pub mod handoff;
// V0.6.1 F139 — embedded `~/.ccteam/hooks/hook.sh` dispatcher + install
// helper. Routes Claude Code hooks through the long-running daemon's
// HTTP server for a ~20× latency reduction.
pub mod hooks_dispatcher;
pub mod inbox;
pub mod memory_bridge;
pub mod meta_agent;
// V0.4.2 F74 — one-shot migration (V0.4.1 → V0.4.2 config.yaml fold).
pub mod migration;
// V0.6.0 Wave 2 F114 — rule-based NL intent → ExecutionMode inferrer
// used by the `ccteam-creator` skill's Phase 2.
pub mod mode_inferrer;
pub mod orchestrator;
pub mod paths;
pub mod pending_inject;
// V0.6.1 F98 — plan-approval ↔ outbox engine. Pure state machine over
// `<project>/.ccteam/plans/*.md` + IM decision strings; emits
// `plan_pending` / `plan_decision` / `plan_timeout` to progress.jsonl.
pub mod plan_approval;
// V0.6.0 Wave 3 F112 §C — `~/.ccteam/preferences.toml` user-opt-in
// fallback knobs (vendor swap on Claude quota exceed).
pub mod preferences;
// V0.6.0 Wave 3 F112 §B — auto-critic vendor decision (used by the
// `ccteam-creator` skill Phase 3.5).
pub mod auto_critic;
pub mod plugin_resolution;
pub mod progress;
pub mod projects;
pub mod queries;
pub mod screenshot;
pub mod silence_classifier;
pub mod skill;
// V0.6.0 F115 — spawn-brief template renderer
// (`{{include_prev_handoffs}}` token).
pub mod spawn_brief;
pub mod stall;
pub mod state;
pub mod team;
pub mod team_resolver;
// V0.5.0 F95 — Anthropic Agent Teams config/inbox/task parsers (pure
// diff helpers). The wiring into the daemon-level watcher lives in
// `artifact_watcher::AgentTeamsWatcher`; these modules are kept
// IO-free so unit tests can hammer the diff logic with fixtures.
pub mod teams_config_parser;
pub mod teams_inbox_parser;
pub mod teams_task_parser;
pub mod templates;
pub mod tmux;
pub mod tool_surface;
// V0.5.0 F92 — cumulative-cost scanner over Claude Code transcript JSONLs.
pub mod transcript_scanner;
pub mod watchdog;
// V0.4.0 F63 — workflow.yaml schema + parser. Pure data + validation;
// no IO side effects beyond reading the YAML file. See module docs.
pub mod workflow;
// V0.4.6 F82 — workflow.yaml file watcher (hot-reload trigger).
pub mod workflow_watcher;

pub use actions::{
    inject_decision, next_inbox_seq, pause, resume, send_to_session, send_to_session_with,
    DecisionInput, SendOptions, SendResult,
};
// V0.6.5 F152 + F153 — advise_vote / advise_parallel entry points
// (used by the `mcp__ccteam__advise_*` MCP dispatch in ccteam-cli).
pub use advise::{
    advise_parallel, advise_vote, append_budget_ledger_row,
    append_budget_sample as append_advise_budget_sample,
    budget_ledger_path as advise_budget_ledger_path, load_budget_ledger as load_advise_budget,
    sum_advise_today, sum_advise_today_by_vendor, AdviseBudgetLedger, AdviseError, Agreement,
    AnswerStatus, BudgetSample, BudgetSnapshot, CodexStatus, ParallelResult, VendorAnswer,
    VoteResult, APPROX_COST_PER_CALL_USD as APPROX_ADVISE_COST_USD, DEFAULT_ADVISE_BUDGET_USD_24H,
    DEFAULT_CODEX_TIMEOUT_SECS,
};
pub use auto_loop::{AutoLoopDecision, AutoLoopFrontMatter, AutoLoopState};
pub use claude_job::{
    classify as classify_job_state, gc_terminated_jobs, gc_user_claude_jobs, probe_job,
    probe_state_json, GcDisposition, GcEntry, GcReport, JobLiveness,
};
#[cfg(any(test, feature = "test-util"))]
pub use claude_job::{link_scan_warn_count, reset_link_scan_warn_for_tests};
pub use config::{
    append_project as append_project_to_config, config_path as ccteam_config_path,
    default_claude_jobs_retention_days, load as load_ccteam_config,
    lookup_project as lookup_project_in_config, remove_project as remove_project_from_config,
    save as save_ccteam_config, upsert_project as upsert_project_in_config, CcteamConfig,
    ProjectEntry, CONFIG_FILENAME,
};
// V0.6.3 F142 — `trigger: schedule` cron evaluation.
pub use cron::{Schedule, ScheduleError};
// V0.6.0 Wave 1 — cost classification moved to `ccteam-cost`. Re-export
// for V0.5.x callers; the new signature is
// `classify(cost, soft_warn, hard_kill)` (primitives, not `&ProjectState`).
pub use ccteam_cost::{classify as classify_cost, CostLevel, COST_MID_WARN_USD};
pub use daemon::{
    check_health as check_daemon_health, check_health_at as check_daemon_health_at, heartbeat_path,
    pidfile_path, read_pidfile, remove_heartbeat, remove_pidfile, send_sigterm_to_pidfile,
    write_heartbeat, write_pidfile, DaemonHealth, HEARTBEAT_GRACE, HEARTBEAT_INTERVAL,
    HEARTBEAT_NAME, PIDFILE_NAME,
};
pub use defaults::{
    claude_jobs_dir_from_env, state_json_path as claude_state_json_path, CLAUDE_JOBS_DIR_ENV,
    DEFAULT_CLAUDE_SID, DEFAULT_TURN_TIMEOUT_SECS,
};
// HarnessAdapter and its cross-vendor types live in ccteam-harness.
// `UnifiedTokenUsage` is still re-exported below via
// `ccteam_cost::{..., UnifiedTokenUsage as Usage}`.
// V0.6.0 F107 — adapter impls. Public so consumers (orchestrator,
// `ccteam-cli` commands) can wire them by concrete type when needed.
pub use execution::{ClaudeTuiAdapter, CodexExecAdapter};
// V0.6.0 F115 — handoff doc mechanism.
pub use handoff::{
    handoff_path, handoffs_dir, list_handoffs, read_concat as read_handoffs_concat, write_handoff,
    WriteHandoffOptions, DEFAULT_INCLUDE_LAST_N as DEFAULT_HANDOFF_INCLUDE_LAST_N,
    HANDOFFS_DIRNAME, HANDOFF_TEMPLATE,
};
// V0.6.1 F139 — `~/.ccteam/hooks/hook.sh` dispatcher install entry.
pub use hooks_dispatcher::{install_hooks, InstallHooksAction, HOOK_DISPATCHER_SH};
// V0.6.0 F115 — spawn-brief template renderer.
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
    migrate_v041_to_v042, migrate_workflow_to_ccteam_dir, render_migration_report,
    render_workflow_migration_report, MigrationReport as V042MigrationReport,
    WorkflowMigrationAction, WorkflowMigrationReport,
};
pub use orchestrator::MAX_CONCURRENT_PROJECTS;
pub use orchestrator::{
    CancelReason, Orchestrator, OrchestratorConfig, TeamEvent, DEFAULT_CLAUDE_MODEL,
};
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
pub use spawn_brief::{render_spawn_brief, SpawnContext as SpawnBriefContext};
// V0.6.0 Wave 1 — pricing moved to `ccteam-cost` with dual-vendor
// (Anthropic + OpenAI) tables. The V0.5.x `Usage` type was renamed
// `UnifiedTokenUsage`; alias here so V0.5 callers reading
// `ccteam_core::Usage` keep compiling.
pub use agent_naming::{pick_unused_bot_name, SCIENTIST_NAMES};
pub use ccteam_cost::{
    estimate_cost, pricing_schema_version, pricing_schema_version_for, ModelPrices,
    UnifiedTokenUsage as Usage, Vendor,
};
pub use mode_inferrer::{infer_mode, CreatorMode, InferenceResult, Intent, Presence, Timeline};
pub use progress::{
    current_agent_sessions, escalation_count, read_all_events, workflow_cost_total,
    AgentSessionStatus, AgentSessionSummary,
};
pub use projects::{
    bootstrap_project, bootstrap_project_at_dir, pick_unused_slug, pick_unused_slug_verbatim,
    pre_trust_project, refuses_active_session, slugify, slugify_brief, validate_slug_format,
    ActiveSessionRefusal,
};
pub use queries::{
    active_sessions, artifact_queue, artifact_status, collect_projects, collect_recent_events,
    compute_cost_summary, cost_history_buckets, cost_summary, cost_summary_from_events,
    count_agent_spawns_within, job_log_tail, workflow_summary, ActiveSessionInfo, AgentStatus,
    ArtifactQueueEntry, ArtifactStatusGroup, CostHistoryBucket, CostSummary, ProjectSummary,
    WorkflowSummary,
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
    install_ccteam_control_skill, install_ccteam_creator_skill, install_ccteam_scan_skill,
    install_ccteam_team_skill, install_into as install_skill_into, install_skill_body_into,
    InstallSkillOptions, InstallSkillReport, SkillInstallAction, CCTEAM_CONTROL_SKILL_NAME,
    CCTEAM_CREATOR_SKILL_NAME, CCTEAM_SCAN_SKILL_NAME, CCTEAM_TEAM_SKILL_NAME, LEGACY_SKILL_NAMES,
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
pub use team_resolver::{
    default_user_staging_dir, discover_team_names, resolve_team, save_team, TeamResolveContext,
    TeamSource, TEAM_SOURCES,
};
pub use templates::{
    apply_probe_defaults_to_workflow_ctx, current_ccteam_bin, default_workflow_ctx,
    merge_project_mcp_json, probe_project, render_project_mcp_json, render_project_settings,
    render_project_settings_agent_team, render_squad_roster_en, render_squad_roster_zh,
    render_workflow_agents_block, render_workflow_template, write_global_helper_templates,
    write_project_settings, write_project_settings_agent_team, EnabledPluginsSetting, Language,
    ProjectKind, ProjectProbe, SettingsEnv, TeammateInfo, WorkflowAgentEntry, WorkflowPreset,
    WorkflowTemplateCtx, WorkflowTemplateRenderError, CCTEAM_MCP_SERVER_KEY, HELPER_TEMPLATES,
    PROJECT_SETTINGS_AGENT_TEAM_JSON, PROJECT_SETTINGS_JSON,
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
    remove_cost_accumulate_hooks, rewrite_legacy_hook_commands, user_claude_dir,
    CostAccumulateScrubAction, CostAccumulateScrubReport, HookCmdRewriteAction,
    HookCmdRewriteReport, LegacySkillAction, LegacySkillReport, MigrationReport, MissingTool,
    ToolSurfaceSnapshot, ToolsRequired, BUILTIN_SUBAGENTS,
};
pub use transcript_scanner::{resolve_jsonl_path, session_cost_from_jsonl};
pub use watchdog::{
    config_path as watchdog_config_path, load_config as load_watchdog_config,
    push_alert_to_meta_outbox as push_watchdog_alert_to_meta_outbox, scan as watchdog_scan,
    AlertKind as WatchdogAlertKind, NotifyMode as WatchdogNotifyMode, WatchdogAlert,
    WatchdogConfig, DEFAULT_NOTIFY_ON_CYCLE_COUNT, WATCHDOG_CONFIG_FILENAME,
};
// V0.4.0 F63 — workflow.yaml schema. V0.4.6 F84 adds `BudgetSpec`.
// V0.5.0 F93b adds `WorkflowMode` / `AgentTeamSpec` / `SuggestedTeammate`.
// V0.5.0 F97 adds `CleanupOnStop`.
pub use workflow::{
    AgentSpec, AgentTeamSpec, BudgetSpec, CleanupOnStop, Executor, OnTimeout, SuggestedTeammate,
    SuggestedTeammateKind, Trigger, WorkflowError, WorkflowMode, WorkflowSpec,
};
// V0.4.6 F82 — workflow.yaml file watcher.
pub use workflow_watcher::{
    WorkflowFileEvent, WorkflowFileEventKind, WorkflowFileWatcher,
    DEBOUNCE_WINDOW as WORKFLOW_WATCHER_DEBOUNCE_WINDOW,
};
// V0.4.0 F64 — artifact watcher event types + watcher entry point.
pub use artifact_watcher::{ArtifactEvent, ArtifactWatcher, WatchKind, DEBOUNCE_WINDOW};
// V0.5.0 F95 — global Anthropic Agent Teams watcher entry points.
pub use artifact_watcher::{AgentTeamsWatcher, AgentTeamsWatcherConfig, TEAMS_DISCOVERY_INTERVAL};
pub use paths::{agent_tasks_root, agent_teams_root, teams_progress_path};

/// Crate version, identical to the workspace package version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
