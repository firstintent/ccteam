//! Watchdog — translation layer that surfaces project anomalies as
//! meta-agent notifications. **Never mutates orchestrator state.**
//!
//! V0.2 M0.21. The watchdog is a smart-layer translator: it reads
//! existing telemetry (per-project `progress.jsonl` / `state.json`,
//! `<project>/.ccteam/needs_attention.outbox.json`, the orchestrator
//! daemon heartbeat) and emits zero or more `WatchdogAlert`s describing
//! what a meta-agent should ask the user about. It does not call
//! orchestrator APIs, never kills sessions, never re-injects prompts —
//! that's the orchestrator's job (tech-design red line: "smart layer
//! only translates, never decides").
//!
//! Four signal sources, in detection order:
//! 1. `<project>/.ccteam/needs_attention.outbox.json` (M0.19 Stop hook
//!    L3 fail-safe — recursion guard)
//! 2. `auto-loop.state.md::iteration >= notify_on_cycle_count`
//!    (auto-loop has spent at least `cycle_count` re-feeds without
//!    completing)
//! 3. phase cost / duration thresholds — any `state.json` with
//!    `cost_used_usd > notify_on_phase_cost_usd` or current phase
//!    older than `notify_on_phase_duration_min`
//! 4. orchestrator daemon heartbeat stale (M0.23.1)
//!
//! User config lives at `~/.ccteam/watchdog.yaml`; missing file ⇒ defaults.
//! `notify_mode: quiet` suppresses everything except `daemon_down` and
//! cost-overrun alerts (those are too important to silence).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use crate::auto_loop;
use crate::daemon::{self, DaemonHealth};
use crate::inbox::{
    outbox_filename, OutboxEventKind, OutboxFrontMatter, OutboxMessage, OutboxPriority,
    LATEST_SCHEMA_VERSION,
};
use crate::meta_agent::meta_slug;
use crate::paths::CcteamPaths;
use crate::state::ProjectState;

/// Default value of `notify_on_cycle_count`. Set to one less than the
/// usual `auto_loop_max_iterations` (3) so the watchdog fires *before*
/// the loop exhausts its retries.
pub const DEFAULT_NOTIFY_ON_CYCLE_COUNT: u32 = 2;

/// Filename under `~/.ccteam/` where user config lives.
pub const WATCHDOG_CONFIG_FILENAME: &str = "watchdog.yaml";

/// User-facing config (`~/.ccteam/watchdog.yaml`). Missing file ⇒
/// `WatchdogConfig::default()`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WatchdogConfig {
    /// Fire an alert once `auto-loop.state.md::iteration` reaches this
    /// number. Default `DEFAULT_NOTIFY_ON_CYCLE_COUNT` (= 2).
    #[serde(default = "default_cycle_count")]
    pub notify_on_cycle_count: u32,

    /// Fire an alert when `state.json::cost_used_usd` exceeds this many
    /// USD. `None` ⇒ rely on the project's existing `cost_policy` only.
    #[serde(default)]
    pub notify_on_phase_cost_usd: Option<f64>,

    /// Fire an alert when the current phase has been in flight longer
    /// than this many minutes (measured from `last_progress_event_at`).
    /// `None` ⇒ disabled.
    #[serde(default)]
    pub notify_on_phase_duration_min: Option<u32>,

    /// Verbosity. `quiet` suppresses cycle/duration alerts but never
    /// silences `daemon_down` or cost-overrun (those are urgent enough
    /// to break through). `verbose` is the same as `normal` plus the
    /// `needs_attention` Stop-hook fallback gets surfaced even if the
    /// meta-agent has already been notified.
    #[serde(default)]
    pub notify_mode: NotifyMode,
}

fn default_cycle_count() -> u32 {
    DEFAULT_NOTIFY_ON_CYCLE_COUNT
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotifyMode {
    /// Suppress non-urgent alerts. `daemon_down` and `cost_overrun`
    /// still fire — they signal real damage if ignored.
    Quiet,
    /// Default. Fires every alert kind once per scan.
    #[default]
    Normal,
    /// Same as `Normal` but does not deduplicate — every scan emits a
    /// `needs_attention` alert even if the file hasn't changed since
    /// the previous scan.
    Verbose,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            notify_on_cycle_count: DEFAULT_NOTIFY_ON_CYCLE_COUNT,
            notify_on_phase_cost_usd: None,
            notify_on_phase_duration_min: None,
            notify_mode: NotifyMode::default(),
        }
    }
}

/// Resolve `<root>/watchdog.yaml`.
pub fn config_path(paths: &CcteamPaths) -> PathBuf {
    paths.root.join(WATCHDOG_CONFIG_FILENAME)
}

/// Load `watchdog.yaml`. Returns `WatchdogConfig::default()` when the
/// file is absent. Parse errors fail-loud — a typo in user config
/// shouldn't silently revert to defaults.
pub fn load_config(paths: &CcteamPaths) -> Result<WatchdogConfig> {
    let path = config_path(paths);
    load_config_at(&path)
}

/// Testable inner — load from an explicit path.
pub fn load_config_at(path: &Path) -> Result<WatchdogConfig> {
    if !path.exists() {
        return Ok(WatchdogConfig::default());
    }
    let body = std::fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?;
    if body.trim().is_empty() {
        return Ok(WatchdogConfig::default());
    }
    serde_yaml::from_str(&body)
        .with_context(|| format!("parse {}", path.display()))
}

/// Discriminator for `WatchdogAlert`. The variant name is the only
/// piece tests / docs reference, so it's stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertKind {
    /// Stop hook L3 fail-safe wrote `needs_attention.outbox.json`.
    NeedsAttention,
    /// Auto-loop iteration crossed `notify_on_cycle_count`.
    AutoLoopCycle,
    /// Project cost exceeded `notify_on_phase_cost_usd`.
    CostOverrun,
    /// Current phase duration exceeded `notify_on_phase_duration_min`.
    PhaseDurationOverrun,
    /// Orchestrator daemon heartbeat is stale or missing.
    DaemonDown,
}

impl AlertKind {
    /// Whether `quiet` mode should still surface this alert. Cost and
    /// daemon-down are too consequential to silence.
    pub fn breaks_through_quiet(self) -> bool {
        matches!(self, AlertKind::CostOverrun | AlertKind::DaemonDown)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            AlertKind::NeedsAttention => "needs_attention",
            AlertKind::AutoLoopCycle => "auto_loop_cycle",
            AlertKind::CostOverrun => "cost_overrun",
            AlertKind::PhaseDurationOverrun => "phase_duration_overrun",
            AlertKind::DaemonDown => "daemon_down",
        }
    }
}

/// One alert emitted by the watchdog. Carries enough context for the
/// meta-agent to ask the user a useful question, but does not commit to
/// any action — the meta-agent decides what to do with it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WatchdogAlert {
    pub kind: AlertKind,
    /// Project slug; `None` for cross-project alerts (only `daemon_down`
    /// today).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    /// Human-readable explanation. Safe to surface verbatim.
    pub message: String,
    /// When this alert was generated.
    pub emitted_at: DateTime<Utc>,
    /// Free-form details (cycle count, cost, pane tail, etc). Schema
    /// is alert-kind specific; meta-agent only needs the message text
    /// to ask the user, the rest is for triage.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub details: serde_json::Value,
}

/// Top-level scan: walk every project + check daemon health, return
/// every alert that survives `notify_mode` filtering. Stable order:
/// daemon alert first, then projects sorted by slug, then alert kinds
/// in `AlertKind` declaration order.
pub fn scan(paths: &CcteamPaths, config: &WatchdogConfig) -> Result<Vec<WatchdogAlert>> {
    scan_at(paths, config, SystemTime::now(), Utc::now())
}

/// Testable inner — explicit `now` so heartbeat / phase-duration
/// classification is deterministic.
pub fn scan_at(
    paths: &CcteamPaths,
    config: &WatchdogConfig,
    now_system: SystemTime,
    now_utc: DateTime<Utc>,
) -> Result<Vec<WatchdogAlert>> {
    let mut alerts = Vec::new();

    // Signal 4: daemon heartbeat. Cross-project, fires once per scan.
    let heartbeat = daemon::heartbeat_path(paths);
    let health = daemon::check_health_at(&heartbeat, now_system);
    if !health.is_healthy() {
        alerts.push(WatchdogAlert {
            kind: AlertKind::DaemonDown,
            slug: None,
            message: health.describe(),
            emitted_at: now_utc,
            details: serde_json::to_value(HealthDetails::from(&health)).unwrap_or_default(),
        });
    }

    let projects_root = &paths.projects_root;
    if !projects_root.exists() {
        return Ok(filter(alerts, config));
    }

    let mut entries: Vec<(String, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(projects_root)
        .with_context(|| format!("read_dir {}", projects_root.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(slug) = entry.file_name().to_str().map(String::from) else {
            continue;
        };
        entries.push((slug, entry.path()));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    for (slug, project_dir) in entries {
        scan_project(paths, &slug, &project_dir, now_utc, &mut alerts)?;
    }

    Ok(filter(alerts, config))
}

#[derive(Debug, Clone, Serialize)]
struct HealthDetails {
    status: &'static str,
    age_secs: Option<u64>,
}

impl From<&DaemonHealth> for HealthDetails {
    fn from(h: &DaemonHealth) -> Self {
        match h {
            DaemonHealth::Healthy { age_secs } => Self {
                status: "healthy",
                age_secs: Some(*age_secs),
            },
            DaemonHealth::NoHeartbeat => Self {
                status: "no_heartbeat",
                age_secs: None,
            },
            DaemonHealth::Stale { age_secs } => Self {
                status: "stale",
                age_secs: Some(*age_secs),
            },
        }
    }
}

fn scan_project(
    paths: &CcteamPaths,
    slug: &str,
    project_dir: &Path,
    now_utc: DateTime<Utc>,
    alerts: &mut Vec<WatchdogAlert>,
) -> Result<()> {
    // Signal 1: Stop hook L3 fail-safe (M0.19).
    let needs = project_dir.join(".ccteam").join("needs_attention.outbox.json");
    if needs.exists() {
        let body = std::fs::read_to_string(&needs)
            .with_context(|| format!("read {}", needs.display()))?;
        let parsed: serde_json::Value =
            serde_json::from_str(&body).unwrap_or_else(|_| serde_json::json!({"raw": body}));
        let reason = parsed
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("phase produced no PHASE_DONE/ESCALATE/outbox");
        alerts.push(WatchdogAlert {
            kind: AlertKind::NeedsAttention,
            slug: Some(slug.to_string()),
            message: format!("project `{slug}` flagged needs_attention: {reason}"),
            emitted_at: now_utc,
            details: parsed,
        });
    }

    // Signal 2: auto-loop iteration count (read auto-loop.state.md;
    // dev-plan §7 calls this `auto_loop_cycle`, the on-disk field is
    // `iteration`).
    let auto_loop_path = auto_loop::path_in(project_dir);
    if let Some(state) = auto_loop::read(&auto_loop_path)? {
        // Surface the iteration even before we know the user's
        // threshold — `filter` enforces it. This keeps the cycle count
        // visible in the details payload.
        let iteration = state.front.iteration;
        let max = state.front.max_iterations;
        if iteration >= 1 {
            alerts.push(WatchdogAlert {
                kind: AlertKind::AutoLoopCycle,
                slug: Some(slug.to_string()),
                message: format!(
                    "project `{slug}` auto-loop on iteration {iteration}/{max} (signal: {})",
                    state.front.completion_signal,
                ),
                emitted_at: now_utc,
                details: serde_json::json!({
                    "iteration": iteration,
                    "max_iterations": max,
                    "completion_signal": state.front.completion_signal,
                }),
            });
        }
    }

    // Signals 3a + 3b: cost / duration thresholds — read state.json.
    let state_path = paths.project_state(slug);
    let Ok(state) = ProjectState::load(&state_path) else {
        return Ok(());
    };

    // Cost overrun (signal 3a) — emit unconditionally; `filter`
    // applies the threshold.
    alerts.push(WatchdogAlert {
        kind: AlertKind::CostOverrun,
        slug: Some(slug.to_string()),
        message: format!(
            "project `{slug}` cost_used_usd = {:.2} (phase `{}`)",
            state.cost_used_usd,
            display_phase(&state.current_phase),
        ),
        emitted_at: now_utc,
        details: serde_json::json!({
            "cost_used_usd": state.cost_used_usd,
            "phase": state.current_phase,
            "team": state.team,
        }),
    });

    // Phase-duration overrun (signal 3b).
    if let Some(last_event) = state.last_progress_event_at {
        let elapsed = now_utc
            .signed_duration_since(last_event)
            .num_seconds()
            .max(0) as u64;
        alerts.push(WatchdogAlert {
            kind: AlertKind::PhaseDurationOverrun,
            slug: Some(slug.to_string()),
            message: format!(
                "project `{slug}` phase `{}` last event {}s ago",
                display_phase(&state.current_phase),
                elapsed,
            ),
            emitted_at: now_utc,
            details: serde_json::json!({
                "phase": state.current_phase,
                "elapsed_seconds": elapsed,
                "last_event_at": last_event.to_rfc3339_opts(SecondsFormat::Secs, true),
            }),
        });
    }

    Ok(())
}

fn display_phase(phase: &str) -> &str {
    if phase.is_empty() {
        "<idle>"
    } else {
        phase
    }
}

/// Apply `notify_mode` + per-kind thresholds to a candidate list.
fn filter(alerts: Vec<WatchdogAlert>, config: &WatchdogConfig) -> Vec<WatchdogAlert> {
    alerts
        .into_iter()
        .filter(|a| keep(a, config))
        .collect()
}

fn keep(alert: &WatchdogAlert, config: &WatchdogConfig) -> bool {
    let quiet = matches!(config.notify_mode, NotifyMode::Quiet);
    match alert.kind {
        AlertKind::DaemonDown => true,
        AlertKind::CostOverrun => {
            // Threshold gate: drop unless cost crosses configured limit.
            // Quiet mode doesn't suppress (cost is the user's money).
            let Some(limit) = config.notify_on_phase_cost_usd else {
                return false;
            };
            let cost = alert
                .details
                .get("cost_used_usd")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            cost > limit
        }
        AlertKind::PhaseDurationOverrun => {
            if quiet {
                return false;
            }
            let Some(limit_min) = config.notify_on_phase_duration_min else {
                return false;
            };
            let elapsed = alert
                .details
                .get("elapsed_seconds")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            elapsed > (limit_min as u64) * 60
        }
        AlertKind::AutoLoopCycle => {
            if quiet {
                return false;
            }
            let iteration = alert
                .details
                .get("iteration")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            iteration >= config.notify_on_cycle_count
        }
        AlertKind::NeedsAttention => !quiet,
    }
}

/// Render an alert into a meta-agent outbox file under
/// `~/projects/meta-<user-handle>/.ccteam/outbox/`. Returns the path
/// written. The meta-agent's own session reads its outbox and surfaces
/// each entry in NL.
///
/// `event_kind` is `Escalation` for `DaemonDown` / `CostOverrun` /
/// `NeedsAttention` (the user has to see them) and `Progress` for the
/// softer `AutoLoopCycle` / `PhaseDurationOverrun` (they describe state,
/// no action mandated).
pub fn push_alert_to_meta_outbox(
    paths: &CcteamPaths,
    user_handle: &str,
    alert: &WatchdogAlert,
) -> Result<PathBuf> {
    let slug = meta_slug(user_handle)?;
    let outbox_dir = paths
        .project_ccteam_dir(&slug)
        .join("outbox");
    std::fs::create_dir_all(&outbox_dir)
        .with_context(|| format!("create {}", outbox_dir.display()))?;

    let now = Utc::now();
    let seq = next_outbox_seq(&outbox_dir, now)?;
    let filename = outbox_filename(now, seq);
    let path = outbox_dir.join(filename);

    let event_kind = match alert.kind {
        AlertKind::DaemonDown | AlertKind::CostOverrun | AlertKind::NeedsAttention => {
            OutboxEventKind::Escalation
        }
        AlertKind::AutoLoopCycle | AlertKind::PhaseDurationOverrun => OutboxEventKind::Progress,
    };
    let priority = match alert.kind {
        AlertKind::DaemonDown | AlertKind::CostOverrun => OutboxPriority::High,
        _ => OutboxPriority::Normal,
    };

    let body = render_alert_body(alert);
    let msg = OutboxMessage {
        front: OutboxFrontMatter {
            schema_version: LATEST_SCHEMA_VERSION,
            in_reply_to: None,
            in_reply_to_source_msg_id: None,
            target_channels: Vec::new(),
            created_at: now,
            priority,
            event_kind,
        },
        body,
    };
    msg.save(&path)?;
    Ok(path)
}

/// Compute the next 1-based sequence number that produces a fresh
/// filename for `now`. Scans the outbox dir for the same compact
/// timestamp prefix and bumps past the highest existing seq. Falls back
/// to `001` when none exists. The caller atomic-writes; ignoring this
/// would race a concurrent meta-agent reply, but watchdog is the only
/// caller that runs as a Rust process so a single-process bump is fine.
fn next_outbox_seq(dir: &Path, now: DateTime<Utc>) -> Result<u32> {
    let prefix_ts = now.to_rfc3339_opts(SecondsFormat::Secs, true).replace(':', "");
    let prefix = format!("reply-{prefix_ts}-");
    let mut max = 0u32;
    if dir.exists() {
        for entry in std::fs::read_dir(dir)
            .with_context(|| format!("read_dir {}", dir.display()))?
        {
            let Ok(entry) = entry else { continue };
            let Some(name) = entry.file_name().to_str().map(String::from) else {
                continue;
            };
            let Some(rest) = name.strip_prefix(&prefix) else {
                continue;
            };
            let Some(seq_str) = rest.strip_suffix(".md") else {
                continue;
            };
            if let Ok(n) = seq_str.parse::<u32>() {
                if n > max {
                    max = n;
                }
            }
        }
    }
    Ok(max + 1)
}

fn render_alert_body(alert: &WatchdogAlert) -> String {
    let mut body = format!("# watchdog: {}\n\n{}\n", alert.kind.as_str(), alert.message);
    if let Some(slug) = &alert.slug {
        body.push_str(&format!("\n- project: `{slug}`\n"));
    }
    body.push_str(&format!(
        "- emitted_at: {}\n",
        alert.emitted_at.to_rfc3339_opts(SecondsFormat::Secs, true),
    ));
    if !alert.details.is_null() {
        body.push_str("\n## details\n\n```json\n");
        body.push_str(
            &serde_json::to_string_pretty(&alert.details)
                .unwrap_or_else(|_| alert.details.to_string()),
        );
        body.push_str("\n```\n");
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auto_loop::AutoLoopState;
    use std::time::Duration;
    use tempfile::TempDir;

    fn paths(tmp: &TempDir) -> CcteamPaths {
        CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        }
    }

    fn write_state(p: &CcteamPaths, slug: &str, mutate: impl FnOnce(&mut ProjectState)) {
        let dir = p.project_ccteam_dir(slug);
        std::fs::create_dir_all(&dir).unwrap();
        let mut s = ProjectState::initial(slug.to_string());
        mutate(&mut s);
        s.save(&p.project_state(slug)).unwrap();
    }

    #[test]
    fn load_config_returns_defaults_when_missing() {
        let tmp = TempDir::new().unwrap();
        let p = paths(&tmp);
        let cfg = load_config(&p).unwrap();
        assert_eq!(cfg, WatchdogConfig::default());
        assert_eq!(cfg.notify_on_cycle_count, 2);
        assert!(cfg.notify_on_phase_cost_usd.is_none());
        assert_eq!(cfg.notify_mode, NotifyMode::Normal);
    }

    #[test]
    fn load_config_parses_user_overrides() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("home")).unwrap();
        let path = tmp.path().join("home").join("watchdog.yaml");
        std::fs::write(
            &path,
            "notify_on_cycle_count: 5\nnotify_on_phase_cost_usd: 10.5\nnotify_mode: quiet\n",
        )
        .unwrap();
        let cfg = load_config_at(&path).unwrap();
        assert_eq!(cfg.notify_on_cycle_count, 5);
        assert_eq!(cfg.notify_on_phase_cost_usd, Some(10.5));
        assert_eq!(cfg.notify_mode, NotifyMode::Quiet);
    }

    #[test]
    fn load_config_fails_loud_on_garbled_yaml() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("watchdog.yaml");
        std::fs::write(&path, "notify_on_cycle_count: not-a-number\n").unwrap();
        assert!(load_config_at(&path).is_err());
    }

    #[test]
    fn quiet_mode_silences_auto_loop_alert_but_not_daemon_down() {
        assert!(AlertKind::DaemonDown.breaks_through_quiet());
        assert!(AlertKind::CostOverrun.breaks_through_quiet());
        assert!(!AlertKind::AutoLoopCycle.breaks_through_quiet());
        assert!(!AlertKind::NeedsAttention.breaks_through_quiet());
        assert!(!AlertKind::PhaseDurationOverrun.breaks_through_quiet());
    }

    #[test]
    fn scan_emits_daemon_down_when_heartbeat_missing() {
        let tmp = TempDir::new().unwrap();
        let p = paths(&tmp);
        std::fs::create_dir_all(&p.projects_root).unwrap();
        let cfg = WatchdogConfig::default();
        let alerts = scan(&p, &cfg).unwrap();
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].kind, AlertKind::DaemonDown);
        assert!(alerts[0].slug.is_none());
    }

    #[test]
    fn scan_skips_daemon_down_when_heartbeat_fresh() {
        let tmp = TempDir::new().unwrap();
        let p = paths(&tmp);
        std::fs::create_dir_all(&p.projects_root).unwrap();
        crate::daemon::write_heartbeat(&p).unwrap();
        let cfg = WatchdogConfig::default();
        let alerts = scan(&p, &cfg).unwrap();
        assert!(
            alerts.iter().all(|a| a.kind != AlertKind::DaemonDown),
            "fresh heartbeat must not trip daemon-down: {alerts:?}",
        );
    }

    #[test]
    fn scan_surfaces_needs_attention_outbox() {
        let tmp = TempDir::new().unwrap();
        let p = paths(&tmp);
        crate::daemon::write_heartbeat(&p).unwrap();
        let slug = "dev-x";
        write_state(&p, slug, |_| {});
        let needs = p
            .project_ccteam_dir(slug)
            .join("needs_attention.outbox.json");
        std::fs::write(
            &needs,
            r#"{"schema_version":1,"slug":"dev-x","reason":"stalled cold"}"#,
        )
        .unwrap();
        let cfg = WatchdogConfig::default();
        let alerts = scan(&p, &cfg).unwrap();
        let needs_alerts: Vec<_> = alerts
            .iter()
            .filter(|a| a.kind == AlertKind::NeedsAttention)
            .collect();
        assert_eq!(needs_alerts.len(), 1);
        assert_eq!(needs_alerts[0].slug.as_deref(), Some(slug));
        assert!(needs_alerts[0].message.contains("stalled cold"));
    }

    #[test]
    fn scan_filters_auto_loop_below_cycle_threshold() {
        let tmp = TempDir::new().unwrap();
        let p = paths(&tmp);
        crate::daemon::write_heartbeat(&p).unwrap();
        let slug = "dev-y";
        write_state(&p, slug, |_| {});
        // iteration=1 < default threshold 2 ⇒ no alert.
        let alp = auto_loop::path_in(&p.project_dir(slug));
        let mut s = AutoLoopState::new(slug.into(), "fix".into(), 3, "DONE".into());
        s.front.iteration = 1;
        auto_loop::write(&alp, &s).unwrap();
        let cfg = WatchdogConfig::default();
        let alerts = scan(&p, &cfg).unwrap();
        assert!(alerts.iter().all(|a| a.kind != AlertKind::AutoLoopCycle));
    }

    #[test]
    fn scan_emits_auto_loop_at_threshold() {
        let tmp = TempDir::new().unwrap();
        let p = paths(&tmp);
        crate::daemon::write_heartbeat(&p).unwrap();
        let slug = "dev-y";
        write_state(&p, slug, |_| {});
        let alp = auto_loop::path_in(&p.project_dir(slug));
        let mut s = AutoLoopState::new(slug.into(), "fix".into(), 3, "DONE".into());
        s.front.iteration = 2;
        auto_loop::write(&alp, &s).unwrap();
        let cfg = WatchdogConfig::default();
        let alerts = scan(&p, &cfg).unwrap();
        let cycle: Vec<_> = alerts
            .iter()
            .filter(|a| a.kind == AlertKind::AutoLoopCycle)
            .collect();
        assert_eq!(cycle.len(), 1);
        assert_eq!(cycle[0].slug.as_deref(), Some(slug));
        assert!(cycle[0].message.contains("2/3"));
    }

    #[test]
    fn quiet_mode_suppresses_auto_loop_but_not_cost_overrun() {
        let tmp = TempDir::new().unwrap();
        let p = paths(&tmp);
        crate::daemon::write_heartbeat(&p).unwrap();
        let slug = "dev-z";
        write_state(&p, slug, |s| {
            s.cost_used_usd = 50.0;
            s.current_phase = "implement".into();
        });
        let alp = auto_loop::path_in(&p.project_dir(slug));
        let mut als = AutoLoopState::new(slug.into(), "fix".into(), 3, "DONE".into());
        als.front.iteration = 3;
        auto_loop::write(&alp, &als).unwrap();
        let cfg = WatchdogConfig {
            notify_mode: NotifyMode::Quiet,
            notify_on_phase_cost_usd: Some(10.0),
            ..WatchdogConfig::default()
        };
        let alerts = scan(&p, &cfg).unwrap();
        assert!(alerts.iter().all(|a| a.kind != AlertKind::AutoLoopCycle));
        assert!(alerts.iter().any(|a| a.kind == AlertKind::CostOverrun));
    }

    #[test]
    fn cost_overrun_threshold_gates() {
        let tmp = TempDir::new().unwrap();
        let p = paths(&tmp);
        crate::daemon::write_heartbeat(&p).unwrap();
        let slug = "dev-cheap";
        write_state(&p, slug, |s| s.cost_used_usd = 5.0);
        // No threshold ⇒ no alert.
        let cfg = WatchdogConfig::default();
        let alerts = scan(&p, &cfg).unwrap();
        assert!(alerts.iter().all(|a| a.kind != AlertKind::CostOverrun));
        // Threshold below cost ⇒ alert.
        let cfg = WatchdogConfig {
            notify_on_phase_cost_usd: Some(1.0),
            ..WatchdogConfig::default()
        };
        let alerts = scan(&p, &cfg).unwrap();
        assert!(alerts.iter().any(|a| a.kind == AlertKind::CostOverrun));
        // Threshold above cost ⇒ no alert.
        let cfg = WatchdogConfig {
            notify_on_phase_cost_usd: Some(10.0),
            ..WatchdogConfig::default()
        };
        let alerts = scan(&p, &cfg).unwrap();
        assert!(alerts.iter().all(|a| a.kind != AlertKind::CostOverrun));
    }

    #[test]
    fn phase_duration_overrun_uses_last_event_age() {
        let tmp = TempDir::new().unwrap();
        let p = paths(&tmp);
        crate::daemon::write_heartbeat(&p).unwrap();
        let slug = "dev-slow";
        let then = Utc::now() - chrono::Duration::minutes(45);
        write_state(&p, slug, |s| {
            s.last_progress_event_at = Some(then);
            s.current_phase = "implement".into();
        });
        let cfg = WatchdogConfig {
            notify_on_phase_duration_min: Some(30),
            ..WatchdogConfig::default()
        };
        let alerts = scan(&p, &cfg).unwrap();
        let dur: Vec<_> = alerts
            .iter()
            .filter(|a| a.kind == AlertKind::PhaseDurationOverrun)
            .collect();
        assert_eq!(dur.len(), 1);
        assert_eq!(dur[0].slug.as_deref(), Some(slug));
    }

    #[test]
    fn push_alert_to_meta_outbox_writes_canonical_outbox_file() {
        let tmp = TempDir::new().unwrap();
        let p = paths(&tmp);
        // Pre-create the meta-agent outbox dir; push doesn't bootstrap
        // the meta project, just writes outbox files.
        let alert = WatchdogAlert {
            kind: AlertKind::DaemonDown,
            slug: None,
            message: "daemon down".into(),
            emitted_at: Utc::now(),
            details: serde_json::json!({"status": "no_heartbeat"}),
        };
        let path = push_alert_to_meta_outbox(&p, "rob", &alert).unwrap();
        assert!(path.exists());
        assert!(
            path.starts_with(p.project_ccteam_dir("meta-rob").join("outbox")),
            "watchdog should write to canonical meta slug; got {}",
            path.display(),
        );
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("watchdog: daemon_down"));
        assert!(body.contains("event_kind: escalation"));
        assert!(body.contains("priority: high"));
        // Round-trip parse.
        let m = OutboxMessage::load(&path).unwrap();
        assert_eq!(m.front.event_kind, OutboxEventKind::Escalation);
        assert_eq!(m.front.priority, OutboxPriority::High);
    }

    #[test]
    fn push_alert_uses_progress_priority_for_soft_alerts() {
        let tmp = TempDir::new().unwrap();
        let p = paths(&tmp);
        let alert = WatchdogAlert {
            kind: AlertKind::AutoLoopCycle,
            slug: Some("dev-q".into()),
            message: "loop on 2/3".into(),
            emitted_at: Utc::now(),
            details: serde_json::Value::Null,
        };
        let path = push_alert_to_meta_outbox(&p, "rob", &alert).unwrap();
        let m = OutboxMessage::load(&path).unwrap();
        assert_eq!(m.front.event_kind, OutboxEventKind::Progress);
        assert_eq!(m.front.priority, OutboxPriority::Normal);
    }

    #[test]
    fn push_alert_to_meta_outbox_increments_seq_within_same_second() {
        let tmp = TempDir::new().unwrap();
        let p = paths(&tmp);
        let alert = WatchdogAlert {
            kind: AlertKind::DaemonDown,
            slug: None,
            message: "x".into(),
            emitted_at: Utc::now(),
            details: serde_json::Value::Null,
        };
        let p1 = push_alert_to_meta_outbox(&p, "rob", &alert).unwrap();
        let p2 = push_alert_to_meta_outbox(&p, "rob", &alert).unwrap();
        assert_ne!(p1, p2, "second push must produce a distinct filename");
    }

    #[test]
    fn scan_at_uses_provided_now_for_heartbeat_classification() {
        // Independent confirmation that scan_at honors the deterministic
        // `now` clock — important because tests above relied on the
        // implicit assumption.
        let tmp = TempDir::new().unwrap();
        let p = paths(&tmp);
        crate::daemon::write_heartbeat(&p).unwrap();
        let mtime = std::fs::metadata(crate::daemon::heartbeat_path(&p))
            .unwrap()
            .modified()
            .unwrap();
        // Far past the grace ⇒ stale.
        let stale_now = mtime + Duration::from_secs(600);
        let alerts = scan_at(&p, &WatchdogConfig::default(), stale_now, Utc::now()).unwrap();
        assert!(alerts.iter().any(|a| a.kind == AlertKind::DaemonDown));
    }
}
