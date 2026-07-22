//! ccteam-core: shared schemas, state/query helpers, project primitives,
//! and hook-shared logic. Runtime workflow orchestration lives in
//! `ccteam-flow`.
//!
//! V0.4.0 F60: the phase machinery modules (`phases`, `golden_rules`,
//! `dag`, `subskill`) and the team-template loaders have been deleted.
//! F66 rebuilt dispatch against the new `workflow.yaml` shape (F63)
//! and artifact-trigger watcher (F64); those runtime pieces now live in
//! `ccteam-flow`.

pub mod actions;
// V0.6.1 F128 — file-mutation helpers for `/ccteam-control
// change-persona` + `add-tool` MCP admin tools. Pure IO over a chat
// bot's `.claude/agents/<bot>.md` definition file.
pub mod admin_actions;
// Budget-ledger helpers only (the advise product surface was removed). Ledger
// file still used by codex critic + doctor cost-orphan rollups.
pub mod advise;
// V0.6.0 Wave 2 F114 — scientist nickname pool used when minting bot
// handles for new chat workflows.
pub mod agent_naming;
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
// v0.8.18 柱1 — OS host identity (hostname) for the `GET /api/v1/hosts` report.
pub mod host;
// v0.8.24 Track D — multi-host registry (join-token / heartbeat / online gate).
pub mod host_registry;
// v0.8.9 Phase 2 — ccteam-hub (curated plugin marketplace) raw-content base
// URL + the pure path/filename utils the installer reuses. Leaf-crate part
// only: the async fetch + sha256-verify + install backend lives in
// `ccteam-im::hub` so this primitives leaf stays free of an async HTTP + sha2
// dependency.
pub mod hub;
// v0.9.7 (PRD F3.1/F3.4) — install-channel detection + lazy latest-version
// check backing `ccteam update` / `ccteam status` / `doctor`.
pub mod install_channel;
pub mod version_check;
// Delegated vendor-plugin install (marketplace pointer → settings.local.json).
pub mod marketplace_plugin;
pub mod model_catalog;
// v0.8.18 柱1 — ccteam's own MCP-server registration into vendor configs
// (Claude `~/.claude.json` + Codex `config.toml`). The ONE allowed write to
// a vendor footprint; the CLI's `mcp_serve` re-exports these seams and the
// web host page calls them for `register-mcp`.
pub mod mcp_register;
// v0.8.6 — generic pull-based hot-reload wrapper for on-disk config
// (stat-on-read, mtime-cached; no file-watch).
pub mod hot_config;
// V0.6.1 F139 — embedded `~/.ccteam/hooks/hook.sh` dispatcher + install
// helper. Routes Claude Code hooks through the long-running daemon's
// HTTP server for a ~20× latency reduction.
pub mod hooks_dispatcher;
pub mod inbox;
pub mod memory_bridge;
// V0.4.2 F74 — one-shot migration (V0.4.1 → V0.4.2 config.yaml fold).
pub mod migration;
// V0.6.0 Wave 2 F114 — rule-based NL intent → ExecutionMode inferrer
// used during project creation (mode inference for new workflows).
pub mod mode_inferrer;
pub mod paths;
pub mod pending_inject;
// V0.6.1 F98 — plan-approval ↔ outbox engine. Pure state machine over
// `<project>/.ccteam/plans/*.md` + IM decision strings; emits
// `plan_pending` / `plan_decision` / `plan_timeout` to progress.jsonl.
pub mod plan_approval;
// V0.6.0 Wave 3 F112 §C — `~/.ccteam/preferences.toml` user-opt-in
// fallback knobs (vendor swap on Claude quota exceed).
pub mod preferences;
// V0.6.0 Wave 3 F112 §B — auto-critic vendor decision (used during
// project creation to pick the critic vendor).
pub mod auto_critic;
pub mod plugin_resolution;
pub mod progress;
pub mod projects;
pub mod queries;
// v0.8.6 W5b ResDisk — read-side reader for project-scoped agent roles
// (`.claude/agents/<role>.md`). Write side lives in `admin_actions`.
pub mod roles;
pub mod screenshot;
// v0.8.7 review-fix (R-M1) — per-session cto-gate secret (mint + ct_eq).
pub mod session_secret;
pub mod silence_classifier;
pub mod skill;
// V0.6.0 F115 — spawn-brief template renderer
// (`{{include_prev_handoffs}}` token).
pub mod spawn_brief;
pub mod stall;
pub mod state;
pub mod team;
pub mod team_resolver;
// v0.8.18 档1 — per-user web tenant registry (web-first user management).
pub mod tenants;
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
pub mod vendor;
pub mod vendor_compat;
pub mod watchdog;

pub use actions::{
    inject_decision, next_inbox_seq, pause, resume, send_to_session, send_to_session_with,
    DecisionInput, SendOptions, SendResult,
};
// V0.6.1 F128 + v0.8.6 W5b — agent .md write primitives. `write_role` is
// the create-or-replace PUT primitive the resource API uses. v0.8.9 Phase 2
// adds the skill sibling `write_skill` (`.claude/skills/<id>/SKILL.md`) used
// by the hub installer.
pub use admin_actions::{
    agent_md_path, change_persona, skill_dir_path, skill_md_path, write_role, write_skill,
    write_skill_file, AddToolResult,
};
// Ledger-only re-exports (product vote/parallel APIs deleted in v0.8.24 C2).
pub use advise::{
    append_budget_ledger_row, append_budget_sample as append_advise_budget_sample,
    append_budget_sample_for_vendor, budget_ledger_path as advise_budget_ledger_path,
    load_budget_ledger as load_advise_budget, sum_advise_today, sum_advise_today_by_agent_vendor,
    sum_advise_today_by_vendor, AdviseBudgetLedger, BudgetSample,
    APPROX_COST_PER_CALL_USD as APPROX_ADVISE_COST_USD, DEFAULT_ADVISE_BUDGET_USD_24H,
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
    default_claude_jobs_retention_days, default_project_host, load as load_ccteam_config,
    lookup_project as lookup_project_in_config, pick_unused_project_slug,
    remove_project as remove_project_from_config, save as save_ccteam_config,
    upsert_project as upsert_project_in_config, CcteamConfig, DelegationConfig, ProjectEntry,
    SessionsConfig, CONFIG_FILENAME,
};
// V0.6.3 F142 — `trigger: schedule` cron evaluation.
pub use cron::{Schedule, ScheduleError};
// V0.6.0 Wave 1 — cost classification moved to `ccteam-cost`. Re-export
// for V0.5.x callers; the new signature is
// `classify(cost, soft_warn, hard_kill)` (primitives, not `&ProjectState`).
pub use ccteam_cost::{classify as classify_cost, CostLevel, COST_MID_WARN_USD};
pub use daemon::{
    acquire_operation_lock, acquire_operation_lock_with_timeout,
    check_health as check_daemon_health, check_health_at as check_daemon_health_at,
    daemon_log_path, daemon_reachable, daemon_socket_path, daemon_status, heartbeat_path,
    operation_lock_path, pidfile_path, probe_daemon, probe_daemon_at, process_exists,
    process_matches_record, read_log_tail, read_pid_record, read_process_start_time,
    remove_heartbeat, start_managed, stop_managed, stop_managed_with, write_heartbeat,
    write_pid_record, DaemonHealth, DaemonProbe, DaemonStartSpec, DaemonStatusReport,
    LifecycleError, OperationLock, PidRecord, StartVerdict, StopTuning, StopVerdict,
    DAEMON_CONNECT_TIMEOUT, DAEMON_LOG_NAME, DAEMON_PROBE_TIMEOUT, HEARTBEAT_GRACE,
    HEARTBEAT_INTERVAL, HEARTBEAT_NAME, MCP_SOCKET_NAME, OPERATION_LOCK_NAME, PIDFILE_NAME,
    START_READY_TIMEOUT, STOP_TERM_WAIT,
};
pub use defaults::{
    claude_jobs_dir_from_env, state_json_path as claude_state_json_path, CLAUDE_BIN_ENV,
    CLAUDE_JOBS_DIR_ENV, CODEX_BIN_ENV, DEFAULT_CLAUDE_SID, DEFAULT_TURN_TIMEOUT_SECS,
    GROK_BIN_ENV, KIMI_BIN_ENV, OPENCODE_BIN_ENV,
};
pub use install_channel::{
    detect as detect_install_channel, install_channel_marker_path, suggested_update_command,
    InstallChannel, InstallMarker, STANDALONE_INSTALL_PIPELINE,
};
pub use version_check::{
    cached_latest, maybe_refresh_latest, update_available, version_cache_path, VersionCache,
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
// v0.8.9 Phase 2 — ccteam-hub raw-content base (curated marketplace) + the two
// pure path/filename utilities the `ccteam_im::hub` installer reuses:
// `raw_url` (joins base + repo-relative path; re-exported as `catalog_raw_url`)
// and `sanitize_role_stem` (normalizes an install stem to `[a-z0-9_-]`).
pub use hub::{raw_url as catalog_raw_url, sanitize_role_stem, HUB_RAW_BASE};
pub use marketplace_plugin::{
    enable_marketplace_plugin, enabled_plugin_key, marketplace_plugin_enabled,
};
// v0.8.6 — generic config hot-reload wrapper (used by the IM gateway for
// config.yaml; reusable for any future config file).
pub use hot_config::HotConfig;
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
pub use migration::{
    migrate_v041_to_v042, migrate_workflow_to_ccteam_dir, render_migration_report,
    render_workflow_migration_report, MigrationReport as V042MigrationReport,
    WorkflowMigrationAction, WorkflowMigrationReport,
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
pub use paths::{
    agent_tasks_root, agent_teams_root, canonical_home_dirs, ensure_ccteam_home,
    teams_progress_path,
};
pub use plan_approval::{PlanApprovalOnTimeout, PlanApprovalSpec};
pub use progress::{
    current_agent_sessions, escalation_count, read_all_events, workflow_cost_total,
    AgentSessionStatus, AgentSessionSummary,
};
pub use projects::{
    bootstrap_project, bootstrap_project_at_dir, ensure_project_data_home, pick_unused_slug,
    pick_unused_slug_verbatim, pre_trust_project, read_current_branch, refuses_active_session,
    slugify, slugify_brief, validate_slug_format, ActiveSessionRefusal,
};
// v0.8.6 W5b ResDisk — read-side role reader for the resource API.
pub use queries::{
    active_sessions, artifact_queue, artifact_status, collect_projects, collect_recent_events,
    compute_cost_summary, cost_history_buckets, cost_summary, cost_summary_from_events,
    count_agent_spawns_within, job_log_tail, workflow_summary, ActiveSessionInfo, AgentStatus,
    ArtifactQueueEntry, ArtifactStatusGroup, CostHistoryBucket, CostSummary, ProjectSummary,
    WorkflowSummary,
};
pub use roles::{
    agents_dir, list_roles, list_skills, read_role, RoleDetail, RoleSummary, SkillSummary,
};
// v0.8.24 Track D — multi-host registry.
pub use host::read_hostname;
pub use host_registry::{
    apply_join, apply_report, gate_remote_spawn, gate_remote_spawn_project, join_tokens_path_in,
    normalize_host_id, now_unix, probe_agents, probe_availability, probe_bin_cached,
    registry_path_in, resolve_bin, AgentProbeSpec, HostAgentReport, HostJoinRequest,
    HostJoinResponse, HostProjectReport, HostRecord, HostRegistry, HostReport, JoinToken,
    JoinTokenStore, SatelliteSelf, VendorAvailability, AGENT_PROBE_SPECS,
    DEFAULT_HEARTBEAT_TTL_SECS, LOCAL_HOST, LOCAL_HOST as REGISTRY_LOCAL_HOST,
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
pub use skill::LEGACY_SKILL_NAMES;
pub use stall::{
    classify as classify_stall, classify_progress_stall, classify_with_thresholds, silent_seconds,
    ProgressStallStatus, StallLevel, StallThresholds, STALL_ESCALATE_SECONDS,
    STALL_SUSPICIOUS_SECONDS, STALL_WARN_SECONDS,
};
pub use state::{Parallelism, PhaseHistoryEntry, PhaseState, ProjectState};
pub use team::{
    CostPolicy, CriticDimensionSpec, CriticStrictness, DomainRule, EscalateGrammarExtension,
    EscalateRoute, GoldenRuleEnforcement, HarnessKind, ProtocolRule, RetroFieldKind,
    RetroFieldSpec, TeamGoldenRules, TeamKind, TeamSpec,
};
pub use team_resolver::{
    default_user_staging_dir, discover_team_names, resolve_team, save_team, TeamResolveContext,
    TeamSource, TEAM_SOURCES,
};
pub use templates::{
    apply_probe_defaults_to_workflow_ctx, current_ccteam_bin, default_workflow_ctx,
    merge_named_mcp_server, merge_project_mcp_json, probe_project, render_project_mcp_json,
    render_project_settings, render_workflow_agents_block, render_workflow_template,
    validate_mcp_server_name, write_global_helper_templates, write_project_settings,
    EnabledPluginsSetting, Language, ProjectKind, ProjectProbe, SettingsEnv, WorkflowAgentEntry,
    WorkflowPreset, WorkflowTemplateCtx, WorkflowTemplateRenderError, CCTEAM_MCP_SERVER_KEY,
    CCTEAM_MCP_SERVE_ARGS, HELPER_TEMPLATES, PROJECT_SETTINGS_JSON,
};
pub use tmux::{
    capture_pane_tail, capture_pane_tail_from_session, capture_pane_with_ansi,
    capture_pane_with_ansi_from_session, pid_is_alive, query_pane_dims,
    query_pane_dims_from_session, session_name_for_project, session_name_for_slug, tmux_available,
    tmux_version, TmuxSession,
};
pub use tool_surface::{
    disable_tool_surface_bootstrap_for_tests, ensure_skills_placeholders,
    migrate_legacy_skill_dirs, migrate_recommended_agent_symlinks, missing_tools,
    remove_chat_hooks, remove_cost_accumulate_hooks, rewrite_legacy_hook_commands, user_claude_dir,
    ChatHookScrubAction, ChatHookScrubReport, CostAccumulateScrubAction, CostAccumulateScrubReport,
    HookCmdRewriteAction, HookCmdRewriteReport, LegacySkillAction, LegacySkillReport,
    MigrationReport, MissingTool, ToolSurfaceSnapshot, ToolsRequired, BUILTIN_SUBAGENTS,
};
pub use transcript_scanner::{resolve_jsonl_path, session_cost_from_jsonl};
pub use vendor::AgentVendor;
pub use vendor_compat::warn_unknown_vendor_token;
pub use watchdog::{
    config_path as watchdog_config_path, load_config as load_watchdog_config,
    push_alert_to_meta_outbox as push_watchdog_alert_to_meta_outbox, scan as watchdog_scan,
    AlertKind as WatchdogAlertKind, NotifyMode as WatchdogNotifyMode, WatchdogAlert,
    WatchdogConfig, DEFAULT_NOTIFY_ON_CYCLE_COUNT, WATCHDOG_CONFIG_FILENAME,
};

/// Crate version, identical to the workspace package version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
