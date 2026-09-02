//! Minimal progress.jsonl helpers used by harness-owned adapters.
//!
//! `ccteam-core` owns the richer query surface, but harness cannot depend
//! on core without reintroducing a cargo cycle. Keep only the small append
//! and row-builder subset needed by execution adapters here.

use std::collections::{BTreeMap, HashMap};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write as _};
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::fs_atomic::atomic_write_durable;
use super::journal;
use crate::ccteam_root_from_env;

type PersistObserver = dyn Fn(&Path, bool) + Send + Sync + 'static;

static PERSIST_OBSERVER: OnceLock<Box<PersistObserver>> = OnceLock::new();

/// Default active progress journal size before single-level rotation.
pub const DEFAULT_PROGRESS_ROTATE_BYTES: u64 = 64 * 1024 * 1024;
const CHECKPOINT_SCHEMA_VERSION: u32 = 1;

pub const CHAT_SESSION_RESET: &str = "chat_session_reset";
pub const CHAT_SESSION_STARTED: &str = "chat_session_started";
pub const CHAT_TURN_USER_PROMPT: &str = "chat_turn_user_prompt";
pub const CHAT_TURN_COMPLETED: &str = "chat_turn_completed";
pub const CHAT_SESSION_RESET_WITH_RECOVERY: &str = "chat_session_reset_with_recovery";
pub const CHAT_COMPACT_DONE: &str = "chat_compact_done";
pub const CHAT_HOP_ESCALATE: &str = "chat_hop_escalate";
pub const CHAT_TOOL_CALL_STARTED: &str = "chat_tool_call_started";
pub const CHAT_BOT_PERMANENT_FAILURE: &str = "chat_bot_permanent_failure";
pub const CHAT_MARKER_SELF_HEAL_ATTEMPT: &str = "chat_marker_self_heal_attempt";
pub const CHAT_TURN_RUNNING_LONG: &str = "chat_turn_running_long";
pub const CHAT_TURN_TIMEOUT: &str = "chat_turn_timeout";
pub const AGENT_DONE: &str = "agent_done";
/// v0.9.2 — a live session was gracefully stopped to admit another session
/// under the daemon-wide capacity limit.
pub const SESSION_EVICTED: &str = "session_evicted";
/// 2026-08-09 — the pump's inbound attachment to a live session ended (the
/// transport under it was replaced, a shared connection was dropped, a child
/// exited). The session is NOT over; the pump is rebuilding the attachment.
pub const SESSION_STREAM_DETACHED: &str = "session_stream_detached";
/// 2026-08-09 — the pump proved a rebuilt attachment by receiving an event on it.
/// Pairs with [`SESSION_STREAM_DETACHED`]; `gap_ms` is the blind window.
pub const SESSION_STREAM_REATTACHED: &str = "session_stream_reattached";
/// 2026-08-19 (one sid, one body) — the daemon let go of a session's OS body
/// without stopping it, or found it still running after a restart. The
/// session is NOT over and NOT driveable by this daemon until the body exits:
/// `reason` = `daemon_shutdown` (graceful stop left it finishing its turn) |
/// `daemon_restart` (found alive by the next daemon).
pub const SESSION_BODY_DETACHED: &str = "session_body_detached";
/// 2026-08-19 — a detached body ended: `reason` = `exited` (finished on its
/// own; `recovered` says whether its unobserved answer was recovered from the
/// vendor's own record) | `stopped` (a user explicitly stopped it). The
/// session is rebuilt by sid right after.
pub const SESSION_BODY_EXITED: &str = "session_body_exited";
/// v0.8.7 review-fix (R-L1) — a HITL session is PARKED awaiting a human
/// approve/deny on a non-allowlist tool call. Emitted when the permission
/// prompt is outstanding so an operator (status / dashboard / `progress`)
/// sees the agent is blocked, not stuck.
pub const CHAT_PERMISSION_PROMPT_OUTSTANDING: &str = "chat_permission_prompt_outstanding";
// v0.9.0 W2 (F2/F5) — delegation lifecycle events. Schema authority for the
// `delegation_*` family lives HERE (progress_bridge); the gateway/dispatch
// layer only calls [`build_delegation_event`] at the corresponding points.
pub const DELEGATION_SPAWNED: &str = "delegation_spawned";
pub const DELEGATION_DISPATCHED: &str = "delegation_dispatched";
pub const DELEGATION_COMPLETED: &str = "delegation_completed";
pub const DELEGATION_NOTIFIED: &str = "delegation_notified";
pub const DELEGATION_COLLECTED: &str = "delegation_collected";
pub const DELEGATION_STOPPED: &str = "delegation_stopped";
pub const DELEGATION_DENIED: &str = "delegation_denied";
/// Card H — a delegation the user's own `pre-agent` policy hook stopped.
/// Distinct from [`DELEGATION_DENIED`] (the engine's built-in depth / fan-out /
/// ceiling / cycle / budget guardrails) because the two are read by different
/// people: this one is answerable by editing a script, and its `reason` tag
/// separates a policy VERDICT (`policy`) from a broken policy script
/// (`policy_script_error`) so a fault is never mistaken for a rule.
pub const DELEGATION_POLICY_DENIED: &str = "delegation_policy_denied";
/// One-shot human-message scheduler lifecycle events.
pub const SCHEDULED_ENQUEUED: &str = "scheduled_enqueued";
pub const SCHEDULED_CANCELLED: &str = "scheduled_cancelled";
pub const SCHEDULED_FIRED: &str = "scheduled_fired";
pub const SCHEDULED_FAILED: &str = "scheduled_failed";
// 2026-09-01 — `ccteam flow run` RUN-LEVEL envelope. Deliberately three kinds
// and no per-agent row: every hire a flow makes is already an ordinary
// delegation on the ledger (`delegation_*`, joined to its run through
// `parent_sid`), so duplicating leaf detail here would put the same fact in two
// places. What lives NOWHERE outside the run directory is the envelope — which
// hires belonged to one run, what that run was called, and how it ended — and
// that is exactly what these three carry. The runner is a CLI process, not the
// daemon, so it submits them through the `flow-run` hook; the schema still
// belongs here, with every other kind.
pub const FLOW_RUN_STARTED: &str = "flow_run_started";
pub const FLOW_RUN_FINISHED: &str = "flow_run_finished";
pub const FLOW_BRAKE_TRIPPED: &str = "flow_brake_tripped";

pub const CODEX_PLAN_UPDATED: &str = "codex_plan_updated";
pub const CODEX_TOKEN_USAGE: &str = "codex_token_usage";
pub const CODEX_THREAD_STATUS: &str = "codex_thread_status";
pub const CODEX_RATE_LIMIT: &str = "codex_rate_limit";
pub const TYPED_EVENT: &str = "typed_event";
pub const MERGER_LOSSY_PARTIAL: &str = "merger_lossy_partial";

/// Every event kind owned by the canonical progress schema.
///
/// Hook fallback and pre-schema rows remain valid unknown facts; they are not
/// promoted into this enum merely because a legacy producer emitted them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventKind {
    ChatSessionReset,
    ChatSessionStarted,
    ChatTurnUserPrompt,
    ChatTurnCompleted,
    ChatSessionResetWithRecovery,
    ChatCompactDone,
    ChatHopEscalate,
    ChatToolCallStarted,
    ChatBotPermanentFailure,
    ChatMarkerSelfHealAttempt,
    ChatTurnRunningLong,
    ChatTurnTimeout,
    AgentDone,
    SessionEvicted,
    SessionStreamDetached,
    SessionStreamReattached,
    SessionBodyDetached,
    SessionBodyExited,
    ChatPermissionPromptOutstanding,
    DelegationSpawned,
    DelegationDispatched,
    DelegationCompleted,
    DelegationNotified,
    DelegationCollected,
    DelegationStopped,
    DelegationDenied,
    DelegationPolicyDenied,
    ScheduledEnqueued,
    ScheduledCancelled,
    ScheduledFired,
    ScheduledFailed,
    FlowRunStarted,
    FlowRunFinished,
    FlowBrakeTripped,
    CodexPlanUpdated,
    CodexTokenUsage,
    CodexThreadStatus,
    CodexRateLimit,
    TypedEvent,
    MergerLossyPartial,
}

impl EventKind {
    pub const ALL: &'static [EventKind] = &[
        EventKind::ChatSessionReset,
        EventKind::ChatSessionStarted,
        EventKind::ChatTurnUserPrompt,
        EventKind::ChatTurnCompleted,
        EventKind::ChatSessionResetWithRecovery,
        EventKind::ChatCompactDone,
        EventKind::ChatHopEscalate,
        EventKind::ChatToolCallStarted,
        EventKind::ChatBotPermanentFailure,
        EventKind::ChatMarkerSelfHealAttempt,
        EventKind::ChatTurnRunningLong,
        EventKind::ChatTurnTimeout,
        EventKind::AgentDone,
        EventKind::SessionEvicted,
        EventKind::SessionStreamDetached,
        EventKind::SessionStreamReattached,
        EventKind::SessionBodyDetached,
        EventKind::SessionBodyExited,
        EventKind::ChatPermissionPromptOutstanding,
        EventKind::DelegationSpawned,
        EventKind::DelegationDispatched,
        EventKind::DelegationCompleted,
        EventKind::DelegationNotified,
        EventKind::DelegationCollected,
        EventKind::DelegationStopped,
        EventKind::DelegationDenied,
        EventKind::DelegationPolicyDenied,
        EventKind::ScheduledEnqueued,
        EventKind::ScheduledCancelled,
        EventKind::ScheduledFired,
        EventKind::ScheduledFailed,
        EventKind::FlowRunStarted,
        EventKind::FlowRunFinished,
        EventKind::FlowBrakeTripped,
        EventKind::CodexPlanUpdated,
        EventKind::CodexTokenUsage,
        EventKind::CodexThreadStatus,
        EventKind::CodexRateLimit,
        EventKind::TypedEvent,
        EventKind::MergerLossyPartial,
    ];

    pub const fn wire_name(self) -> &'static str {
        match self {
            EventKind::ChatSessionReset => CHAT_SESSION_RESET,
            EventKind::ChatSessionStarted => CHAT_SESSION_STARTED,
            EventKind::ChatTurnUserPrompt => CHAT_TURN_USER_PROMPT,
            EventKind::ChatTurnCompleted => CHAT_TURN_COMPLETED,
            EventKind::ChatSessionResetWithRecovery => CHAT_SESSION_RESET_WITH_RECOVERY,
            EventKind::ChatCompactDone => CHAT_COMPACT_DONE,
            EventKind::ChatHopEscalate => CHAT_HOP_ESCALATE,
            EventKind::ChatToolCallStarted => CHAT_TOOL_CALL_STARTED,
            EventKind::ChatBotPermanentFailure => CHAT_BOT_PERMANENT_FAILURE,
            EventKind::ChatMarkerSelfHealAttempt => CHAT_MARKER_SELF_HEAL_ATTEMPT,
            EventKind::ChatTurnRunningLong => CHAT_TURN_RUNNING_LONG,
            EventKind::ChatTurnTimeout => CHAT_TURN_TIMEOUT,
            EventKind::AgentDone => AGENT_DONE,
            EventKind::SessionEvicted => SESSION_EVICTED,
            EventKind::SessionStreamDetached => SESSION_STREAM_DETACHED,
            EventKind::SessionStreamReattached => SESSION_STREAM_REATTACHED,
            EventKind::SessionBodyDetached => SESSION_BODY_DETACHED,
            EventKind::SessionBodyExited => SESSION_BODY_EXITED,
            EventKind::ChatPermissionPromptOutstanding => CHAT_PERMISSION_PROMPT_OUTSTANDING,
            EventKind::DelegationSpawned => DELEGATION_SPAWNED,
            EventKind::DelegationDispatched => DELEGATION_DISPATCHED,
            EventKind::DelegationCompleted => DELEGATION_COMPLETED,
            EventKind::DelegationNotified => DELEGATION_NOTIFIED,
            EventKind::DelegationCollected => DELEGATION_COLLECTED,
            EventKind::DelegationStopped => DELEGATION_STOPPED,
            EventKind::DelegationDenied => DELEGATION_DENIED,
            EventKind::DelegationPolicyDenied => DELEGATION_POLICY_DENIED,
            EventKind::ScheduledEnqueued => SCHEDULED_ENQUEUED,
            EventKind::ScheduledCancelled => SCHEDULED_CANCELLED,
            EventKind::ScheduledFired => SCHEDULED_FIRED,
            EventKind::ScheduledFailed => SCHEDULED_FAILED,
            EventKind::FlowRunStarted => FLOW_RUN_STARTED,
            EventKind::FlowRunFinished => FLOW_RUN_FINISHED,
            EventKind::FlowBrakeTripped => FLOW_BRAKE_TRIPPED,
            EventKind::CodexPlanUpdated => CODEX_PLAN_UPDATED,
            EventKind::CodexTokenUsage => CODEX_TOKEN_USAGE,
            EventKind::CodexThreadStatus => CODEX_THREAD_STATUS,
            EventKind::CodexRateLimit => CODEX_RATE_LIMIT,
            EventKind::TypedEvent => TYPED_EVENT,
            EventKind::MergerLossyPartial => MERGER_LOSSY_PARTIAL,
        }
    }

    pub fn from_wire_name(value: &str) -> Option<Self> {
        Some(match value {
            CHAT_SESSION_RESET => EventKind::ChatSessionReset,
            CHAT_SESSION_STARTED => EventKind::ChatSessionStarted,
            CHAT_TURN_USER_PROMPT => EventKind::ChatTurnUserPrompt,
            CHAT_TURN_COMPLETED => EventKind::ChatTurnCompleted,
            CHAT_SESSION_RESET_WITH_RECOVERY => EventKind::ChatSessionResetWithRecovery,
            CHAT_COMPACT_DONE => EventKind::ChatCompactDone,
            CHAT_HOP_ESCALATE => EventKind::ChatHopEscalate,
            CHAT_TOOL_CALL_STARTED => EventKind::ChatToolCallStarted,
            CHAT_BOT_PERMANENT_FAILURE => EventKind::ChatBotPermanentFailure,
            CHAT_MARKER_SELF_HEAL_ATTEMPT => EventKind::ChatMarkerSelfHealAttempt,
            CHAT_TURN_RUNNING_LONG => EventKind::ChatTurnRunningLong,
            CHAT_TURN_TIMEOUT => EventKind::ChatTurnTimeout,
            AGENT_DONE => EventKind::AgentDone,
            SESSION_EVICTED => EventKind::SessionEvicted,
            SESSION_STREAM_DETACHED => EventKind::SessionStreamDetached,
            SESSION_STREAM_REATTACHED => EventKind::SessionStreamReattached,
            SESSION_BODY_DETACHED => EventKind::SessionBodyDetached,
            SESSION_BODY_EXITED => EventKind::SessionBodyExited,
            CHAT_PERMISSION_PROMPT_OUTSTANDING => EventKind::ChatPermissionPromptOutstanding,
            DELEGATION_SPAWNED => EventKind::DelegationSpawned,
            DELEGATION_DISPATCHED => EventKind::DelegationDispatched,
            DELEGATION_COMPLETED => EventKind::DelegationCompleted,
            DELEGATION_NOTIFIED => EventKind::DelegationNotified,
            DELEGATION_COLLECTED => EventKind::DelegationCollected,
            DELEGATION_STOPPED => EventKind::DelegationStopped,
            DELEGATION_DENIED => EventKind::DelegationDenied,
            DELEGATION_POLICY_DENIED => EventKind::DelegationPolicyDenied,
            SCHEDULED_ENQUEUED => EventKind::ScheduledEnqueued,
            SCHEDULED_CANCELLED => EventKind::ScheduledCancelled,
            SCHEDULED_FIRED => EventKind::ScheduledFired,
            SCHEDULED_FAILED => EventKind::ScheduledFailed,
            FLOW_RUN_STARTED => EventKind::FlowRunStarted,
            FLOW_RUN_FINISHED => EventKind::FlowRunFinished,
            FLOW_BRAKE_TRIPPED => EventKind::FlowBrakeTripped,
            CODEX_PLAN_UPDATED => EventKind::CodexPlanUpdated,
            CODEX_TOKEN_USAGE => EventKind::CodexTokenUsage,
            CODEX_THREAD_STATUS => EventKind::CodexThreadStatus,
            CODEX_RATE_LIMIT => EventKind::CodexRateLimit,
            TYPED_EVENT => EventKind::TypedEvent,
            MERGER_LOSSY_PARTIAL => EventKind::MergerLossyPartial,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventScope {
    Project,
    Session,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventClass {
    Fact,
    LatestState {
        min_interval: Duration,
        scope: EventScope,
    },
    Telemetry,
}

/// Classify every schema-owned kind. Deliberately no wildcard: a new
/// [`EventKind`] cannot compile until its persistence policy is chosen.
pub const fn class(kind: EventKind) -> EventClass {
    match kind {
        EventKind::ChatSessionReset
        | EventKind::ChatSessionStarted
        | EventKind::ChatTurnUserPrompt
        | EventKind::ChatTurnCompleted
        | EventKind::ChatSessionResetWithRecovery
        | EventKind::ChatCompactDone
        | EventKind::ChatHopEscalate
        | EventKind::ChatToolCallStarted
        | EventKind::ChatBotPermanentFailure
        | EventKind::ChatMarkerSelfHealAttempt
        | EventKind::ChatTurnTimeout
        | EventKind::AgentDone
        | EventKind::SessionEvicted
        | EventKind::SessionStreamDetached
        | EventKind::SessionStreamReattached
        | EventKind::SessionBodyDetached
        | EventKind::SessionBodyExited
        | EventKind::ChatPermissionPromptOutstanding
        | EventKind::DelegationSpawned
        | EventKind::DelegationDispatched
        | EventKind::DelegationCompleted
        | EventKind::DelegationNotified
        | EventKind::DelegationCollected
        | EventKind::DelegationStopped
        | EventKind::DelegationDenied
        | EventKind::DelegationPolicyDenied
        | EventKind::ScheduledEnqueued
        | EventKind::ScheduledCancelled
        | EventKind::ScheduledFired
        | EventKind::ScheduledFailed
        // Flow-run envelope rows are one-off lifecycle facts, exactly like the
        // `delegation_*` family: each says a thing HAPPENED at a point in time
        // (a run opened, a brake tripped, a run ended) rather than restating a
        // value that keeps changing. Collapsing them under `LatestState` would
        // be wrong twice over — two concurrent runs of the same project would
        // suppress each other, and losing the `started` row would leave a
        // `finished` row that no reader can attribute to a run.
        | EventKind::FlowRunStarted
        | EventKind::FlowRunFinished
        | EventKind::FlowBrakeTripped
        | EventKind::CodexPlanUpdated
        | EventKind::TypedEvent
        | EventKind::MergerLossyPartial => EventClass::Fact,
        EventKind::CodexTokenUsage | EventKind::CodexThreadStatus | EventKind::CodexRateLimit => {
            EventClass::LatestState {
                min_interval: Duration::from_secs(30),
                scope: EventScope::Project,
            }
        }
        EventKind::ChatTurnRunningLong => EventClass::LatestState {
            min_interval: Duration::from_secs(5 * 60),
            scope: EventScope::Session,
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KindStat {
    pub kind: String,
    pub unknown: bool,
    pub appended_count: u64,
    pub appended_bytes: u64,
    pub suppressed_count: u64,
    pub suppressed_bytes: u64,
}

/// Stable identity for the single retained `<slug>.1.jsonl` archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveCoverage {
    /// Exact archive byte length.
    pub byte_size: u64,
    /// SHA-256 of the first raw line, including its newline when present.
    pub first_line_sha256: Option<String>,
}

/// Cumulative lifetime aggregates for data no longer present in the active
/// progress journal.
///
/// Per-sid cost totals are retained because they are effectively free while
/// streaming the archive and make the checkpoint useful to future per-session
/// consumers. Rolling 24-hour minute buckets deliberately remain active-file
/// only; after rotation they can undercount pre-rotation minutes, while every
/// lifetime field here remains exact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProgressCheckpoint {
    /// Checkpoint wire schema.
    pub schema_version: u32,
    /// Monotonic count of archives folded into this checkpoint.
    pub rotation_sequence: u64,
    /// Parseable events folded across all rotated-away generations.
    pub event_count: u64,
    /// Corrupt byte-lines observed across rotated-away generations.
    pub corrupt_line_count: u64,
    /// Lifetime `agent_done.cost_usd` total.
    pub cost_total_usd: f64,
    /// Lifetime cost grouped by event vendor.
    pub cost_total_by_vendor: BTreeMap<String, f64>,
    /// Lifetime cost grouped by `sid` (falling back to legacy `session_id`).
    pub cost_total_by_sid: BTreeMap<String, f64>,
    /// The current `.1` archive already included in these cumulative totals.
    pub coverage: Option<ArchiveCoverage>,
}

impl Default for ProgressCheckpoint {
    fn default() -> Self {
        Self {
            schema_version: CHECKPOINT_SCHEMA_VERSION,
            rotation_sequence: 0,
            event_count: 0,
            corrupt_line_count: 0,
            cost_total_usd: 0.0,
            cost_total_by_vendor: BTreeMap::new(),
            cost_total_by_sid: BTreeMap::new(),
            coverage: None,
        }
    }
}

/// Shared cost fields extracted from one canonical progress event.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProgressCostContribution<'a> {
    /// Numeric `cost_usd` on an `agent_done` row (missing means zero).
    pub cost_usd: f64,
    /// Optional vendor label from the same row.
    pub vendor: Option<&'a str>,
    /// Optional canonical or legacy session id from the same row.
    pub sid: Option<&'a str>,
}

/// Result of repairing one corrupt active or archive journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgressRepairReport {
    /// Parseable records preserved.
    pub kept_count: u64,
    /// Corrupt byte-lines removed.
    pub dropped_count: u64,
    /// Durable backup containing the exact original bytes.
    pub backup_path: PathBuf,
}

#[derive(Debug, Clone, Copy, Default)]
struct KindCounters {
    unknown: bool,
    appended_count: u64,
    appended_bytes: u64,
    suppressed_count: u64,
    suppressed_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct AdmissionKey {
    path: PathBuf,
    kind: EventKind,
    scope: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct PersistedState {
    hash: [u8; 32],
    at: Instant,
}

#[derive(Debug, Clone, Copy)]
struct PendingState {
    hash: [u8; 32],
    reservation: u64,
}

#[derive(Debug, Default)]
struct LatestStateEntry {
    persisted: Option<PersistedState>,
    pending: Option<PendingState>,
}

#[derive(Debug, Default)]
struct AdmissionState {
    canonical_paths: HashMap<PathBuf, PathBuf>,
    latest: HashMap<AdmissionKey, LatestStateEntry>,
    stats: HashMap<String, KindCounters>,
    next_reservation: u64,
}

static ADMISSION_STATE: OnceLock<Mutex<AdmissionState>> = OnceLock::new();

fn admission_state() -> MutexGuard<'static, AdmissionState> {
    ADMISSION_STATE
        .get_or_init(|| Mutex::new(AdmissionState::default()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Snapshot process-global admission counters, sorted by kind for stable
/// doctor/metrics output.
pub fn kind_stats() -> Vec<KindStat> {
    let state = admission_state();
    let mut stats = state
        .stats
        .iter()
        .map(|(kind, counters)| KindStat {
            kind: kind.clone(),
            unknown: counters.unknown,
            appended_count: counters.appended_count,
            appended_bytes: counters.appended_bytes,
            suppressed_count: counters.suppressed_count,
            suppressed_bytes: counters.suppressed_bytes,
        })
        .collect::<Vec<_>>();
    stats.sort_unstable_by(|left, right| left.kind.cmp(&right.kind));
    stats
}

/// Install the process-wide callback invoked after a progress row is durably
/// appended. The boolean marks an append that also rotated the active file.
/// Returns `false` when another daemon component already installed the
/// callback. One-shot CLI processes never call this function.
pub fn set_persist_observer(observer: Box<PersistObserver>) -> bool {
    PERSIST_OBSERVER.set(observer).is_ok()
}

pub fn hooks_script_from_env() -> Option<PathBuf> {
    ccteam_root_from_env().map(|root| root.join("hooks").join("hook.sh"))
}

pub fn progress_jsonl_from_env(slug: &str) -> Option<PathBuf> {
    ccteam_root_from_env().map(|root| {
        root.join("state")
            .join("progress")
            .join(format!("{slug}.jsonl"))
    })
}

/// Resolve the rotation threshold.
///
/// `CCTEAM_PROGRESS_ROTATE_BYTES` is an operational/test override. Unset,
/// non-numeric, and zero values all fall back to the 64 MiB default so a bad
/// environment cannot accidentally turn every append into a rotation.
pub fn progress_rotate_bytes() -> u64 {
    std::env::var("CCTEAM_PROGRESS_ROTATE_BYTES")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_PROGRESS_ROTATE_BYTES)
}

/// Derive the single retained archive path for an active progress journal.
pub fn progress_archive_path(active_path: &Path) -> PathBuf {
    progress_sibling_path(active_path, ".1.jsonl")
}

/// Derive the lifetime checkpoint path for an active progress journal.
pub fn progress_checkpoint_path(active_path: &Path) -> PathBuf {
    progress_sibling_path(active_path, ".checkpoint.json")
}

/// Read a progress checkpoint without mutating or recovering it.
pub fn read_progress_checkpoint(active_path: &Path) -> Result<Option<ProgressCheckpoint>> {
    let path = progress_checkpoint_path(active_path);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let checkpoint = serde_json::from_slice::<ProgressCheckpoint>(&bytes)
        .with_context(|| format!("parse {}", path.display()))?;
    if checkpoint.schema_version != CHECKPOINT_SCHEMA_VERSION {
        anyhow::bail!(
            "unsupported progress checkpoint schema {} in {}",
            checkpoint.schema_version,
            path.display()
        );
    }
    Ok(Some(checkpoint))
}

/// Compute the coverage marker for an archive, or `None` when it is absent.
pub fn progress_archive_coverage(active_path: &Path) -> Result<Option<ArchiveCoverage>> {
    archive_coverage_for_path(&progress_archive_path(active_path))
}

/// Return whether a parsed checkpoint covers the current `.1` archive.
pub fn checkpoint_covers_archive(
    checkpoint: &ProgressCheckpoint,
    archive: Option<&ArchiveCoverage>,
) -> bool {
    checkpoint.coverage.as_ref() == archive
}

/// Load the lifetime checkpoint and close the crash window where active was
/// renamed to `.1` but its aggregates were not checkpointed yet.
///
/// Recovery uses the same stable flock as append/rotation, streams `.1` once,
/// and atomically replaces the checkpoint. A covered archive is never scanned.
pub fn load_or_recover_progress_checkpoint(
    active_path: &Path,
) -> Result<Option<ProgressCheckpoint>> {
    // Read-only callers (projection catch-up runs this for EVERY slug) must
    // not materialize the lock/dir for a project that has no progress state
    // at all — otherwise a mere query mints `.lock` droppings.
    if !active_path.exists()
        && !progress_archive_path(active_path).exists()
        && !progress_checkpoint_path(active_path).exists()
    {
        return Ok(None);
    }
    if let Some(parent) = active_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let lock_file = open_progress_lock(active_path)?;
    let _lock = ProgressFileLock::lock(&lock_file)
        .with_context(|| format!("lock {}", progress_lock_path(active_path).display()))?;
    recover_progress_checkpoint_locked(active_path)
}

/// Extract the one lifetime cost formula shared by projection and checkpoint
/// folding. Only `agent_done` contributes; missing/non-numeric cost is zero.
pub fn progress_cost_contribution(event: &Value) -> Option<ProgressCostContribution<'_>> {
    (event_kind_name(event) == Some(AGENT_DONE)).then(|| ProgressCostContribution {
        cost_usd: event.get("cost_usd").and_then(Value::as_f64).unwrap_or(0.0),
        vendor: event.get("vendor").and_then(Value::as_str),
        sid: event
            .get("sid")
            .and_then(Value::as_str)
            .or_else(|| event.get("session_id").and_then(Value::as_str)),
    })
}

pub fn append_event(path: &Path, event: &Value) -> Result<()> {
    append_event_at(path, event, Instant::now(), None)
}

fn append_event_at(
    path: &Path,
    event: &Value,
    now: Instant,
    min_interval_override: Option<Duration>,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }

    let mut line = Vec::new();
    serde_json::to_writer(&mut line, event).context("serialize progress event")?;
    line.push(b'\n');
    let byte_count = u64::try_from(line.len()).unwrap_or(u64::MAX);

    let raw_kind = event_kind_name(event);
    let known_kind = raw_kind.and_then(EventKind::from_wire_name);
    let kind_name = raw_kind.unwrap_or("<unknown>");
    let unknown = known_kind.is_none();

    // Warn once per unknown kind per process: hook-fallback kinds are
    // legitimate high-volume facts, and a per-event warn would itself be
    // the log-spam this gate exists to remove. The stats map gains an
    // entry after the first record_* call, so its absence marks first sight.
    if unknown && !admission_state().stats.contains_key(kind_name) {
        tracing::warn!(
            kind = kind_name,
            "progress admission: unknown event kind persisted as a fact"
        );
    }

    let event_class = known_kind.map(class).unwrap_or(EventClass::Fact);
    let reservation = match event_class {
        EventClass::Fact => None,
        EventClass::Telemetry => {
            record_suppressed(kind_name, unknown, byte_count);
            return Ok(());
        }
        EventClass::LatestState {
            min_interval,
            scope,
        } => {
            let content_payload = semantic_content_payload(event);
            if semantic_payload_is_all_null(&content_payload) {
                record_suppressed(kind_name, unknown, byte_count);
                return Ok(());
            }

            let key = AdmissionKey {
                path: canonical_admission_path(path)?,
                kind: known_kind.expect("latest-state classes are schema-owned"),
                scope: match scope {
                    EventScope::Project => None,
                    EventScope::Session => {
                        event.get("sid").and_then(Value::as_str).map(str::to_owned)
                    }
                },
            };
            let hash = semantic_hash(event)?;
            let min_interval = min_interval_override.unwrap_or(min_interval);
            match reserve_latest(key, hash, now, min_interval) {
                Some(reservation) => Some(reservation),
                None => {
                    record_suppressed(kind_name, unknown, byte_count);
                    return Ok(());
                }
            }
        }
    };

    let result = append_serialized(path, &line);
    if let Some(reservation) = reservation {
        finish_latest(reservation, result.is_ok(), now);
    }
    let rotated = result?;
    record_appended(kind_name, unknown, byte_count);
    if let Some(observer) = PERSIST_OBSERVER.get() {
        observer(path, rotated);
    }
    Ok(())
}

fn event_kind_name(event: &Value) -> Option<&str> {
    event
        .get("event")
        .and_then(Value::as_str)
        .or_else(|| event.get("kind").and_then(Value::as_str))
}

fn append_serialized(path: &Path, line: &[u8]) -> Result<bool> {
    let lock_file = open_progress_lock(path)?;
    let _lock = ProgressFileLock::lock(&lock_file)
        .with_context(|| format!("lock {}", progress_lock_path(path).display()))?;

    // A real crash in the rename -> checkpoint window leaves `.1` present and
    // active absent. Recover before accepting another row so an uncovered
    // archive can never survive until a later rotation replaces it.
    if !path.exists() && progress_archive_path(path).exists() {
        recover_progress_checkpoint_locked(path)?;
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    file.write_all(line)
        .with_context(|| format!("write event to {}", path.display()))?;
    let size = file
        .metadata()
        .with_context(|| format!("stat {}", path.display()))?
        .len();
    drop(file);

    if size > progress_rotate_bytes() {
        rotate_progress_locked(path)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

fn progress_sibling_path(active_path: &Path, suffix: &str) -> PathBuf {
    let file_name = active_path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    let stem = file_name.strip_suffix(".jsonl").unwrap_or(&file_name);
    active_path.with_file_name(format!("{stem}{suffix}"))
}

fn progress_lock_path(active_path: &Path) -> PathBuf {
    progress_sibling_path(active_path, ".lock")
}

fn open_progress_lock(active_path: &Path) -> Result<File> {
    let path = progress_lock_path(active_path);
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))
}

fn rotate_progress_locked(active_path: &Path) -> Result<()> {
    let archive_path = progress_archive_path(active_path);
    match std::fs::remove_file(&archive_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("remove {}", archive_path.display()))
        }
    }
    std::fs::rename(active_path, &archive_path).with_context(|| {
        format!(
            "rotate {} -> {}",
            active_path.display(),
            archive_path.display()
        )
    })?;

    // Crash consistency is marker based: rename first, then fold the now
    // immutable archive and atomically publish the checkpoint. A crash between
    // these operations leaves a marker mismatch that startup hydration repairs.
    recover_progress_checkpoint_locked(active_path)?;
    File::create(active_path).with_context(|| format!("create {}", active_path.display()))?;
    Ok(())
}

fn recover_progress_checkpoint_locked(active_path: &Path) -> Result<Option<ProgressCheckpoint>> {
    let archive_path = progress_archive_path(active_path);
    let archive = archive_coverage_for_path(&archive_path)?;
    let mut checkpoint = read_progress_checkpoint(active_path)?;
    let Some(archive) = archive else {
        return Ok(checkpoint);
    };
    if checkpoint
        .as_ref()
        .is_some_and(|checkpoint| checkpoint_covers_archive(checkpoint, Some(&archive)))
    {
        return Ok(checkpoint);
    }

    let mut next = checkpoint.take().unwrap_or_default();
    let summary = journal::scan_stream(&archive_path, |event| {
        if let Some(cost) = progress_cost_contribution(&event) {
            next.cost_total_usd += cost.cost_usd;
            if let Some(vendor) = cost.vendor {
                *next
                    .cost_total_by_vendor
                    .entry(vendor.to_string())
                    .or_insert(0.0) += cost.cost_usd;
            }
            if let Some(sid) = cost.sid {
                *next.cost_total_by_sid.entry(sid.to_string()).or_insert(0.0) += cost.cost_usd;
            }
        }
        next.event_count = next.event_count.saturating_add(1);
    })?;
    next.corrupt_line_count = next
        .corrupt_line_count
        .saturating_add(u64::try_from(summary.corrupt_count).unwrap_or(u64::MAX));
    next.rotation_sequence = next.rotation_sequence.saturating_add(1);
    next.coverage = Some(archive);
    write_progress_checkpoint(active_path, &next)?;
    Ok(Some(next))
}

fn write_progress_checkpoint(active_path: &Path, checkpoint: &ProgressCheckpoint) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(checkpoint).context("serialize progress checkpoint")?;
    atomic_write_durable(&progress_checkpoint_path(active_path), &bytes)
}

fn archive_coverage_for_path(path: &Path) -> Result<Option<ArchiveCoverage>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("open {}", path.display())),
    };
    let byte_size = file
        .metadata()
        .with_context(|| format!("stat {}", path.display()))?
        .len();
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut found_bytes = false;
    loop {
        let buffer = reader
            .fill_buf()
            .with_context(|| format!("read first line from {}", path.display()))?;
        if buffer.is_empty() {
            break;
        }
        found_bytes = true;
        let take = buffer
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(buffer.len(), |index| index + 1);
        hasher.update(&buffer[..take]);
        let ended = buffer[take - 1] == b'\n';
        reader.consume(take);
        if ended {
            break;
        }
    }
    let first_line_sha256 = found_bytes.then(|| hex_digest(hasher.finalize().as_slice()));
    Ok(Some(ArchiveCoverage {
        byte_size,
        first_line_sha256,
    }))
}

fn hex_digest(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

/// Repair corrupt byte-lines in the active journal or its `.1` archive.
///
/// The original is first renamed to a timestamped backup and the directory is
/// synced before the repaired tmp is installed. Clean files are left untouched
/// and return `None`, making repeated repair runs idempotent.
pub fn repair_progress_journal(
    active_path: &Path,
    target_path: &Path,
) -> Result<Option<ProgressRepairReport>> {
    let archive_path = progress_archive_path(active_path);
    if target_path != active_path && target_path != archive_path {
        anyhow::bail!(
            "repair target {} is not active/archive for {}",
            target_path.display(),
            active_path.display()
        );
    }
    if let Some(parent) = active_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let lock_file = open_progress_lock(active_path)?;
    let _lock = ProgressFileLock::lock(&lock_file)
        .with_context(|| format!("lock {}", progress_lock_path(active_path).display()))?;
    repair_progress_journal_locked(active_path, target_path)
}

fn repair_progress_journal_locked(
    active_path: &Path,
    target_path: &Path,
) -> Result<Option<ProgressRepairReport>> {
    let input = match File::open(target_path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("open {}", target_path.display())),
    };
    let repairing_archive = target_path == progress_archive_path(active_path);
    let old_coverage = repairing_archive
        .then(|| archive_coverage_for_path(target_path))
        .transpose()?
        .flatten();
    // Checkpoint damage must never prevent byte repair. A parse/I/O error is
    // already a doctor warning; leave that sidecar untouched and repair the
    // journal, updating coverage only when a valid checkpoint covered `.1`.
    let checkpoint = if repairing_archive {
        read_progress_checkpoint(active_path).ok().flatten()
    } else {
        None
    };
    let checkpoint_covered_old = old_coverage.as_ref().is_some_and(|coverage| {
        checkpoint
            .as_ref()
            .is_some_and(|checkpoint| checkpoint.coverage.as_ref() == Some(coverage))
    });

    let tmp_path = unique_maintenance_path(target_path, "repair-tmp");
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&tmp_path)
        .with_context(|| format!("create {}", tmp_path.display()))?;
    let mut reader = BufReader::new(input);
    let mut raw = Vec::new();
    let mut kept_count = 0_u64;
    let mut dropped_count = 0_u64;
    loop {
        raw.clear();
        let read = reader
            .read_until(b'\n', &mut raw)
            .with_context(|| format!("read {}", target_path.display()))?;
        if read == 0 {
            break;
        }
        let line = trim_ascii_bytes(&raw);
        if line.is_empty() {
            continue;
        }
        if serde_json::from_slice::<Value>(line).is_ok() {
            output
                .write_all(&raw)
                .with_context(|| format!("write {}", tmp_path.display()))?;
            if raw.last() != Some(&b'\n') {
                output
                    .write_all(b"\n")
                    .with_context(|| format!("finish line in {}", tmp_path.display()))?;
            }
            kept_count = kept_count.saturating_add(1);
        } else {
            dropped_count = dropped_count.saturating_add(1);
        }
    }

    if dropped_count == 0 {
        drop(output);
        let _ = std::fs::remove_file(&tmp_path);
        return Ok(None);
    }
    output
        .sync_all()
        .with_context(|| format!("fsync {}", tmp_path.display()))?;
    drop(output);
    drop(reader);

    let backup_path = unique_backup_path(target_path);
    std::fs::rename(target_path, &backup_path).with_context(|| {
        format!(
            "back up {} -> {}",
            target_path.display(),
            backup_path.display()
        )
    })?;
    sync_parent_dir(target_path);
    std::fs::rename(&tmp_path, target_path).with_context(|| {
        format!(
            "replace {} from repaired copy {} (original is safe at {})",
            target_path.display(),
            tmp_path.display(),
            backup_path.display()
        )
    })?;
    sync_parent_dir(target_path);

    if checkpoint_covered_old {
        let mut checkpoint = checkpoint.expect("coverage match requires a checkpoint");
        checkpoint.coverage = archive_coverage_for_path(target_path)?;
        write_progress_checkpoint(active_path, &checkpoint)?;
    }

    Ok(Some(ProgressRepairReport {
        kept_count,
        dropped_count,
        backup_path,
    }))
}

fn unique_maintenance_path(path: &Path, suffix: &str) -> PathBuf {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    let stamp = Utc::now().format("%Y%m%dT%H%M%S%.6fZ");
    path.with_file_name(format!("{name}.{suffix}-{stamp}-{}", std::process::id()))
}

fn unique_backup_path(path: &Path) -> PathBuf {
    let candidate = unique_maintenance_path(path, "bak");
    if !candidate.exists() {
        return candidate;
    }
    for sequence in 1_u32.. {
        let numbered = candidate.with_file_name(format!(
            "{}-{sequence}",
            candidate
                .file_name()
                .map(|name| name.to_string_lossy())
                .unwrap_or_default()
        ));
        if !numbered.exists() {
            return numbered;
        }
    }
    unreachable!("u32 backup suffix space exhausted")
}

fn trim_ascii_bytes(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

fn sync_parent_dir(path: &Path) {
    if let Some(parent) = path.parent() {
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
    }
}

#[derive(Debug)]
struct Reservation {
    key: AdmissionKey,
    id: u64,
    hash: [u8; 32],
}

fn reserve_latest(
    key: AdmissionKey,
    hash: [u8; 32],
    now: Instant,
    min_interval: Duration,
) -> Option<Reservation> {
    let mut state = admission_state();
    let entry = state.latest.entry(key.clone()).or_default();

    if entry.pending.is_some() {
        // Only one write for a key may be in flight. Periodic latest-state
        // sources recover a changed value on their next notification.
        return None;
    }
    if entry
        .persisted
        .is_some_and(|persisted| persisted.hash == hash)
    {
        return None;
    }
    if entry
        .persisted
        .is_some_and(|persisted| now.saturating_duration_since(persisted.at) < min_interval)
    {
        // Deliberately do not retain a suppressed change: these state kinds
        // are periodic, so the next event after the interval recovers it.
        return None;
    }

    state.next_reservation = state.next_reservation.wrapping_add(1);
    let id = state.next_reservation;
    state
        .latest
        .get_mut(&key)
        .expect("latest entry was inserted above")
        .pending = Some(PendingState {
        hash,
        reservation: id,
    });
    Some(Reservation { key, id, hash })
}

fn finish_latest(reservation: Reservation, persisted: bool, now: Instant) {
    let mut state = admission_state();
    let Some(entry) = state.latest.get_mut(&reservation.key) else {
        return;
    };
    if entry.pending.is_none_or(|pending| {
        pending.reservation != reservation.id || pending.hash != reservation.hash
    }) {
        return;
    }
    entry.pending = None;
    if persisted {
        entry.persisted = Some(PersistedState {
            hash: reservation.hash,
            at: now,
        });
    } else if entry.persisted.is_none() {
        state.latest.remove(&reservation.key);
    }
}

fn canonical_admission_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("resolve current directory for progress admission")?
            .join(path)
    };
    if let Some(canonical) = admission_state().canonical_paths.get(&absolute).cloned() {
        return Ok(canonical);
    }

    let canonical = if absolute.exists() {
        std::fs::canonicalize(&absolute)
            .with_context(|| format!("canonicalize {}", absolute.display()))?
    } else {
        let parent = absolute.parent().unwrap_or(Path::new("/"));
        let canonical_parent = std::fs::canonicalize(parent)
            .with_context(|| format!("canonicalize {}", parent.display()))?;
        match absolute.file_name() {
            Some(name) => canonical_parent.join(name),
            None => canonical_parent,
        }
    };
    admission_state()
        .canonical_paths
        .insert(absolute, canonical.clone());
    Ok(canonical)
}

fn semantic_hash(event: &Value) -> Result<[u8; 32]> {
    let mut payload = event.clone();
    if let Some(object) = payload.as_object_mut() {
        object.remove("ts");
    }
    let bytes = serde_json::to_vec(&payload).context("serialize semantic progress payload")?;
    Ok(Sha256::digest(bytes).into())
}

fn semantic_content_payload(event: &Value) -> Value {
    const METADATA_FIELDS: &[&str] = &[
        "event",
        "kind",
        "vendor",
        "ts",
        "role",
        "sid",
        "slug",
        "thread_id",
        "turn_id",
        "session",
    ];

    let mut payload = event.clone();
    if let Some(object) = payload.as_object_mut() {
        for field in METADATA_FIELDS {
            object.remove(*field);
        }
    }
    payload
}

/// True when a semantic payload has no non-null leaf. Empty objects/arrays and
/// arbitrarily nested null-only structures are empty state snapshots.
pub fn semantic_payload_is_all_null(payload: &Value) -> bool {
    match payload {
        Value::Null => true,
        Value::Array(values) => values.iter().all(semantic_payload_is_all_null),
        Value::Object(values) => values.values().all(semantic_payload_is_all_null),
        Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

fn record_appended(kind: &str, unknown: bool, bytes: u64) {
    let mut state = admission_state();
    let counters = state.stats.entry(kind.to_string()).or_default();
    counters.unknown |= unknown;
    counters.appended_count = counters.appended_count.saturating_add(1);
    counters.appended_bytes = counters.appended_bytes.saturating_add(bytes);
}

fn record_suppressed(kind: &str, unknown: bool, bytes: u64) {
    let mut state = admission_state();
    let counters = state.stats.entry(kind.to_string()).or_default();
    counters.unknown |= unknown;
    counters.suppressed_count = counters.suppressed_count.saturating_add(1);
    counters.suppressed_bytes = counters.suppressed_bytes.saturating_add(bytes);
}

#[cfg(unix)]
struct ProgressFileLock(std::os::fd::RawFd);

#[cfg(unix)]
impl ProgressFileLock {
    fn lock(file: &std::fs::File) -> std::io::Result<Self> {
        let fd = file.as_raw_fd();
        let rc = unsafe { libc::flock(fd, libc::LOCK_EX) };
        if rc == 0 {
            Ok(Self(fd))
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
}

#[cfg(unix)]
impl Drop for ProgressFileLock {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.0, libc::LOCK_UN) };
    }
}

#[cfg(not(unix))]
struct ProgressFileLock;

#[cfg(not(unix))]
impl ProgressFileLock {
    fn lock(_file: &std::fs::File) -> std::io::Result<Self> {
        Ok(Self)
    }
}

pub fn build_chat_tool_call_started_event(role: &str, tool: &str) -> Value {
    json!({
        "event": CHAT_TOOL_CALL_STARTED,
        "role": role,
        "tool": tool,
        "ts": Utc::now().to_rfc3339(),
    })
}

/// v0.8.7 review-fix (R-L1) — a HITL permission prompt is OUTSTANDING: the
/// session is parked awaiting a human approve/deny for `tool` (`summary` is
/// the one-line tool-call preview). Lets an operator see a parked agent
/// instead of mistaking the silence for a stuck/dead session. `ttl_secs` is
/// the prompt's deadline (deny on lapse — fail-safe).
pub fn build_chat_permission_prompt_outstanding_event(
    role: &str,
    tool: &str,
    summary: &str,
    ttl_secs: u64,
) -> Value {
    let trimmed: String = summary.chars().take(256).collect();
    json!({
        "event": CHAT_PERMISSION_PROMPT_OUTSTANDING,
        "role": role,
        "tool": tool,
        "summary": trimmed,
        "ttl_secs": ttl_secs,
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_chat_session_started_event(role: &str, project_dir: &str) -> Value {
    json!({
        "event": CHAT_SESSION_STARTED,
        "role": role,
        "project_dir": project_dir,
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_chat_turn_user_prompt_event(
    role: &str,
    sid: &str,
    turn_id: &str,
    prompt_excerpt: &str,
) -> Value {
    let trimmed: String = prompt_excerpt.chars().take(256).collect();
    json!({
        "event": CHAT_TURN_USER_PROMPT,
        "role": role,
        "sid": sid,
        "turn_id": turn_id,
        "prompt_excerpt": trimmed,
        "ts": Utc::now().to_rfc3339(),
    })
}

/// `model` is the turn's canonical model id (e.g. `claude-opus-4-8`) for
/// deterministic per-turn cost pricing — written ONLY when present
/// (`Some`); a `None` (e.g. the tmux Stop hook, which carries no model)
/// omits the key so the cost path treats the turn as unpriced (exposed,
/// never billed at a fallback rate).
pub fn build_chat_turn_completed_event(
    role: &str,
    sid: &str,
    turn_id: &str,
    usage: &ccteam_cost::UnifiedTokenUsage,
    model: Option<&str>,
) -> Value {
    let mut ev = json!({
        "event": CHAT_TURN_COMPLETED,
        "role": role,
        "sid": sid,
        "turn_id": turn_id,
        "usage": serde_json::to_value(usage).unwrap_or(Value::Null),
        "ts": Utc::now().to_rfc3339(),
    });
    if let Some(model) = model.filter(|m| !m.is_empty()) {
        ev["model"] = Value::String(model.to_string());
    }
    ev
}

/// Build the terminal success row consumed by progress and cost queries.
/// Keeping the complete `agent_done` shape here makes this module the sole
/// schema authority; adapters only translate vendor events into these fields.
#[allow(clippy::too_many_arguments)]
pub fn build_agent_done_completed_event(
    role: &str,
    session_id: &str,
    slug: &str,
    vendor: &str,
    thread_id: &str,
    turn_id: &str,
    usage: &ccteam_cost::UnifiedTokenUsage,
    cost_usd: Option<f64>,
) -> Value {
    let mut event = json!({
        "event": AGENT_DONE,
        "role": role,
        "session_id": session_id,
        "slug": slug,
        "status": "completed",
        "vendor": vendor,
        "thread_id": thread_id,
        "turn_id": turn_id,
        "usage": serde_json::to_value(usage).unwrap_or(Value::Null),
        "ts": Utc::now().to_rfc3339(),
    });
    if let Some(cost_usd) = cost_usd {
        event["cost_usd"] = json!(cost_usd);
    }
    event
}

/// Build a terminal vendor-error row. `turn_id` is absent for failures that
/// happen before a vendor turn is established (for example, connect errors).
#[allow(clippy::too_many_arguments)]
pub fn build_agent_done_errored_event(
    role: &str,
    session_id: &str,
    slug: &str,
    vendor: &str,
    thread_id: &str,
    turn_id: Option<&str>,
    error_kind: &str,
    error: &str,
) -> Value {
    let mut event = json!({
        "event": AGENT_DONE,
        "role": role,
        "session_id": session_id,
        "slug": slug,
        "status": "errored",
        "vendor": vendor,
        "error_kind": error_kind,
        "error": error,
        "thread_id": thread_id,
        "ts": Utc::now().to_rfc3339(),
    });
    if let Some(turn_id) = turn_id.filter(|value| !value.is_empty()) {
        event["turn_id"] = Value::String(turn_id.to_string());
    }
    event
}

pub fn build_chat_session_reset_event(role: &str, sid: &str) -> Value {
    json!({
        "event": CHAT_SESSION_RESET,
        "role": role,
        "sid": sid,
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_chat_session_reset_event_with_reason(role: &str, sid: &str, reason: &str) -> Value {
    json!({
        "event": CHAT_SESSION_RESET,
        "role": role,
        "sid": sid,
        "reason": reason,
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_chat_session_reset_with_recovery_event(
    role: &str,
    sid: &str,
    recovered_turns: usize,
) -> Value {
    json!({
        "event": CHAT_SESSION_RESET_WITH_RECOVERY,
        "role": role,
        "sid": sid,
        "recovered_turns": recovered_turns,
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_chat_compact_done_event(role: &str) -> Value {
    json!({
        "event": CHAT_COMPACT_DONE,
        "role": role,
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_chat_hop_escalate_event(role: &str, hop_count: u32, last_bot: &str) -> Value {
    json!({
        "event": CHAT_HOP_ESCALATE,
        "role": role,
        "hop_count": hop_count,
        "last_bot": last_bot,
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_chat_bot_permanent_failure_event(role: &str, reason: &str, attempts: u32) -> Value {
    let trimmed: String = reason.chars().take(512).collect();
    json!({
        "event": CHAT_BOT_PERMANENT_FAILURE,
        "role": role,
        "reason": trimmed,
        "attempts": attempts,
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_chat_marker_self_heal_attempt_event(role: &str, attempt_n: u32) -> Value {
    json!({
        "event": CHAT_MARKER_SELF_HEAL_ATTEMPT,
        "role": role,
        "attempt_n": attempt_n,
        "ts": Utc::now().to_rfc3339(),
    })
}

/// The mid-turn "still working" heartbeat. `sid` is REQUIRED: the read-side
/// activity classifier selects a session's latest event by sid, so an untagged
/// heartbeat is invisible to the session it describes (and would leak onto its
/// siblings through the project-tail fallback). Same field order as
/// [`build_chat_turn_timeout_event`] — the two are the busy/stuck ends of the
/// same turn-liveness family.
pub fn build_chat_turn_running_long_event(
    role: &str,
    sid: &str,
    slug: &str,
    turn_id: &str,
    elapsed_sec: u64,
) -> Value {
    json!({
        "event": CHAT_TURN_RUNNING_LONG,
        "role": role,
        "sid": sid,
        "slug": slug,
        "turn_id": turn_id,
        "elapsed_sec": elapsed_sec,
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_chat_turn_timeout_event(
    role: &str,
    sid: &str,
    slug: &str,
    turn_id: &str,
    elapsed_sec: u64,
) -> Value {
    json!({
        "event": CHAT_TURN_TIMEOUT,
        "role": role,
        "sid": sid,
        "slug": slug,
        "turn_id": turn_id,
        "elapsed_sec": elapsed_sec,
        "stuck": true,
        "ts": Utc::now().to_rfc3339(),
    })
}

/// 2026-08-09 — the inbound attachment for `sid` ended while the session is still
/// live. `sid` is REQUIRED (the read-side classifier selects by sid) and the
/// row is deliberately an IDLE boundary: a detached session is not working,
/// and claiming otherwise is the exact lie this family exists to stop.
pub fn build_session_stream_detached_event(
    role: &str,
    sid: &str,
    slug: &str,
    reason: &str,
) -> Value {
    json!({
        "event": SESSION_STREAM_DETACHED,
        "role": role,
        "sid": sid,
        "slug": slug,
        "reason": reason,
        "ts": Utc::now().to_rfc3339(),
    })
}

/// 2026-08-09 — the rebuilt attachment for `sid` delivered its first event.
/// `gap_ms` measures how long the session was unobservable, `attempts` how
/// many rebuilds it took.
pub fn build_session_stream_reattached_event(
    role: &str,
    sid: &str,
    slug: &str,
    gap_ms: u64,
    attempts: u32,
) -> Value {
    json!({
        "event": SESSION_STREAM_REATTACHED,
        "role": role,
        "sid": sid,
        "slug": slug,
        "gap_ms": gap_ms,
        "attempts": attempts,
        "ts": Utc::now().to_rfc3339(),
    })
}

/// v0.9.2 — build the durable capacity-eviction lifecycle event. `reason` is
/// currently `"capacity"`; keeping it explicit leaves the schema extensible
/// without inventing a second event family.
pub fn build_session_evicted_event(sid: &str, reason: &str) -> Value {
    json!({
        "event": SESSION_EVICTED,
        "sid": sid,
        "reason": reason,
        "ts": Utc::now().to_rfc3339(),
    })
}

/// 2026-08-19 — a session's body is running without this daemon reading it.
/// `reason` = `daemon_shutdown` | `daemon_restart`; `pid` is the body; `in_flight`
/// says whether a vendor turn was known to be running when the daemon let go
/// (`None` = not known, e.g. found after a restart).
pub fn build_session_body_detached_event(
    sid: &str,
    slug: &str,
    reason: &str,
    pid: Option<u32>,
    in_flight: Option<bool>,
) -> Value {
    json!({
        "event": SESSION_BODY_DETACHED,
        "sid": sid,
        "slug": slug,
        "reason": reason,
        "pid": pid,
        "in_flight": in_flight,
        "ts": Utc::now().to_rfc3339(),
    })
}

/// 2026-08-19 — a detached body ended. `reason` = `exited` | `stopped`;
/// `recovered` = the unobserved answer was recovered from the vendor's record.
pub fn build_session_body_exited_event(
    sid: &str,
    slug: &str,
    reason: &str,
    pid: u32,
    recovered: bool,
) -> Value {
    json!({
        "event": SESSION_BODY_EXITED,
        "sid": sid,
        "slug": slug,
        "reason": reason,
        "pid": pid,
        "recovered": recovered,
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_typed_event_event(
    vendor: &str,
    event_kind: &str,
    captured: &str,
    session: &str,
) -> Value {
    json!({
        "kind": TYPED_EVENT,
        "vendor": vendor,
        "event_kind": event_kind,
        "captured": captured,
        "session": session,
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_merger_lossy_partial_event(
    vendor: &str,
    event_kind: &str,
    captured: &str,
    session: &str,
) -> Value {
    json!({
        "kind": MERGER_LOSSY_PARTIAL,
        "vendor": vendor,
        "event_kind": event_kind,
        "captured": captured,
        "session": session,
        "ts": Utc::now().to_rfc3339(),
    })
}

/// v0.9.0 W2 — build one `delegation_*` progress event. `event` is one of the
/// `DELEGATION_*` consts. The unified payload is `{parent_sid, child_sid,
/// vendor, host, turn?, title?, reason?}` — optional fields are omitted when
/// `None`/empty so a `delegation_denied{reason}` and a `delegation_spawned`
/// share one shape without null noise.
#[allow(clippy::too_many_arguments)]
pub fn build_delegation_event(
    event: &str,
    parent_sid: &str,
    child_sid: &str,
    vendor: &str,
    host: &str,
    turn: Option<&str>,
    title: Option<&str>,
    reason: Option<&str>,
) -> Value {
    let mut ev = json!({
        "event": event,
        "parent_sid": parent_sid,
        "child_sid": child_sid,
        "vendor": vendor,
        "host": host,
        "ts": Utc::now().to_rfc3339(),
    });
    let obj = ev.as_object_mut().expect("json object");
    if let Some(turn) = turn.filter(|t| !t.is_empty()) {
        obj.insert("turn".to_string(), Value::String(turn.to_string()));
    }
    if let Some(title) = title.filter(|t| !t.is_empty()) {
        obj.insert("title".to_string(), Value::String(title.to_string()));
    }
    if let Some(reason) = reason.filter(|r| !r.is_empty()) {
        obj.insert("reason".to_string(), Value::String(reason.to_string()));
    }
    ev
}

/// Build one scheduled-message lifecycle row. The full message body never
/// enters `progress.jsonl`; callers may supply only a hard-capped preview.
pub fn build_scheduled_event(
    event: &str,
    id: &str,
    sid: &str,
    send_at: &str,
    preview: Option<&str>,
    reason: Option<&str>,
) -> Value {
    let mut ev = json!({
        "event": event,
        "id": id,
        "sid": sid,
        "send_at": send_at,
        "ts": Utc::now().to_rfc3339(),
    });
    let obj = ev.as_object_mut().expect("json object");
    if let Some(preview) = preview.filter(|value| !value.is_empty()) {
        obj.insert(
            "preview".to_string(),
            Value::String(preview.chars().take(80).collect()),
        );
    }
    if let Some(reason) = reason.filter(|value| !value.is_empty()) {
        obj.insert(
            "reason".to_string(),
            Value::String(reason.chars().take(256).collect()),
        );
    }
    ev
}

/// Cap one free-text field a flow script chose the contents of. The ledger is
/// an append-only file every reader tails; a script's `meta.description` (or a
/// brake reason built from a template) must not be able to make one row
/// kilobytes wide. Truncation is by CHARACTER so the row stays valid UTF-8.
fn capped(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

/// A flow run opened. `run_id` is the run directory's basename — stable for
/// the life of the run (a `--resume` continues the SAME id) and therefore the
/// join key between this row, its `flow_run_finished` partner, and the
/// on-disk journal an evaluation reads.
///
/// `started_at` is the RUN's clock, not the daemon's: the row is submitted by
/// a CLI process over HTTP, so `ts` (when the ledger accepted it) and
/// `started_at` (when the run actually began) are different facts and both are
/// kept. `parent_sid` is the managed session that launched the run — the same
/// attribution its hires carry — and is omitted when a flow was started from a
/// plain shell that belongs to no session.
pub fn build_flow_run_started_event(
    run_id: &str,
    project: &str,
    parent_sid: Option<&str>,
    name: &str,
    description: &str,
    script_path: &str,
    started_at: &str,
) -> Value {
    let mut event = json!({
        "event": FLOW_RUN_STARTED,
        "run_id": run_id,
        "project": project,
        "name": capped(name, 120),
        "description": capped(description, 400),
        "script_path": capped(script_path, 512),
        "started_at": started_at,
        "ts": Utc::now().to_rfc3339(),
    });
    if let Some(parent_sid) = parent_sid.filter(|sid| !sid.is_empty()) {
        event["parent_sid"] = Value::String(parent_sid.to_string());
    }
    event
}

/// A flow run ended. `ok` is the runner's own verdict (false when the script
/// threw or a brake ended the run early); `brake` names the brake when one
/// tripped, so a reader can tell "the plan refused to admit more work" from
/// "the plan broke" without replaying the run directory.
pub fn build_flow_run_finished_event(
    run_id: &str,
    project: &str,
    agents: usize,
    cost_usd: f64,
    ok: bool,
    brake: Option<&str>,
    finished_at: &str,
) -> Value {
    let mut event = json!({
        "event": FLOW_RUN_FINISHED,
        "run_id": run_id,
        "project": project,
        "agents": agents,
        "cost_usd": cost_usd,
        "ok": ok,
        "finished_at": finished_at,
        "ts": Utc::now().to_rfc3339(),
    });
    if let Some(brake) = brake.filter(|reason| !reason.is_empty()) {
        event["brake"] = Value::String(capped(brake, 256));
    }
    event
}

/// A brake refused a flow run new admissions. Emitted once, at the moment
/// admission first refuses — in-flight workers keep running, so this row is
/// NOT the end of the run and never replaces `flow_run_finished`.
pub fn build_flow_brake_tripped_event(run_id: &str, project: &str, reason: &str) -> Value {
    json!({
        "event": FLOW_BRAKE_TRIPPED,
        "run_id": run_id,
        "project": project,
        "reason": capped(reason, 256),
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_codex_plan_updated_event(
    thread_id: &str,
    turn_id: &str,
    explanation: Option<&str>,
    plan: Value,
) -> Value {
    let mut v = json!({
        "event": CODEX_PLAN_UPDATED,
        "vendor": "codex",
        "thread_id": thread_id,
        "turn_id": turn_id,
        "plan": plan,
        "ts": Utc::now().to_rfc3339(),
    });
    if let Some(explanation) = explanation {
        v.as_object_mut().unwrap().insert(
            "explanation".to_string(),
            Value::String(explanation.to_string()),
        );
    }
    v
}

pub fn build_codex_token_usage_event(
    thread_id: &str,
    turn_id: &str,
    total: Value,
    last: Value,
    model_context_window: Option<i64>,
) -> Value {
    let mut v = json!({
        "event": CODEX_TOKEN_USAGE,
        "vendor": "codex",
        "thread_id": thread_id,
        "turn_id": turn_id,
        "total": total,
        "last": last,
        "ts": Utc::now().to_rfc3339(),
    });
    if let Some(window) = model_context_window {
        v.as_object_mut().unwrap().insert(
            "model_context_window".to_string(),
            Value::Number(window.into()),
        );
    }
    v
}

pub fn build_codex_thread_status_event(
    thread_id: &str,
    status: &str,
    active_flags: Vec<String>,
) -> Value {
    json!({
        "event": CODEX_THREAD_STATUS,
        "vendor": "codex",
        "thread_id": thread_id,
        "status": status,
        "active_flags": active_flags,
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_codex_rate_limit_event(snapshot: Value) -> Value {
    json!({
        "event": CODEX_RATE_LIMIT,
        "vendor": "codex",
        "snapshot": snapshot,
        "ts": Utc::now().to_rfc3339(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_rows(path: &Path) -> Vec<Value> {
        match std::fs::read_to_string(path) {
            Ok(body) => body
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => panic!("read {}: {error}", path.display()),
        }
    }

    #[test]
    fn append_event_writes_exactly_one_jsonl_record_for_multiline_values() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let event = json!({
            "event": "chat_tool_use",
            "tool": "Bash",
            "cmd": "printf 'a\\nb\\n'",
        });

        append_event(&path, &event).unwrap();

        let body = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<_> = body.lines().collect();
        assert_eq!(lines.len(), 1);
        assert_eq!(serde_json::from_str::<Value>(lines[0]).unwrap(), event);
    }

    #[test]
    fn session_evicted_event_has_minimal_capacity_shape() {
        let event = build_session_evicted_event("s9", "capacity");
        assert_eq!(event["event"], SESSION_EVICTED);
        assert_eq!(event["sid"], "s9");
        assert_eq!(event["reason"], "capacity");
        assert!(event["ts"].is_string());
    }

    #[test]
    fn errored_agent_done_preserves_kind_and_optional_turn() {
        let with_turn = build_agent_done_errored_event(
            "worker",
            "s9",
            "demo",
            "codex",
            "thread-1",
            Some("turn-2"),
            "server_overloaded",
            "at capacity",
        );
        assert_eq!(with_turn["event"], AGENT_DONE);
        assert_eq!(with_turn["status"], "errored");
        assert_eq!(with_turn["error_kind"], "server_overloaded");
        assert_eq!(with_turn["turn_id"], "turn-2");

        let without_turn = build_agent_done_errored_event(
            "worker",
            "s9",
            "demo",
            "codex",
            "thread-1",
            None,
            "connect",
            "connection failed",
        );
        assert!(without_turn.get("turn_id").is_none());
    }

    #[test]
    fn flow_run_kinds_round_trip_their_wire_names_and_persist_as_facts() {
        for kind in [
            EventKind::FlowRunStarted,
            EventKind::FlowRunFinished,
            EventKind::FlowBrakeTripped,
        ] {
            assert_eq!(EventKind::from_wire_name(kind.wire_name()), Some(kind));
            assert!(EventKind::ALL.contains(&kind), "{kind:?} missing from ALL");
            // A run envelope must never be collapsed: two concurrent runs would
            // suppress each other under LatestState.
            assert_eq!(class(kind), EventClass::Fact, "{kind:?}");
        }
    }

    #[test]
    fn flow_run_started_keeps_both_clocks_and_omits_an_absent_parent() {
        let attributed = build_flow_run_started_event(
            "audit-20260901-101500",
            "alpha",
            Some("s42"),
            "audit-routes",
            "Audit route handlers",
            "/w/flow.js",
            "2026-09-01T10:15:00Z",
        );
        assert_eq!(attributed["event"], FLOW_RUN_STARTED);
        assert_eq!(attributed["run_id"], "audit-20260901-101500");
        assert_eq!(attributed["parent_sid"], "s42");
        // `started_at` is the run's clock; `ts` is when the ledger took the row.
        assert_eq!(attributed["started_at"], "2026-09-01T10:15:00Z");
        assert!(attributed["ts"].is_string());

        let from_a_plain_shell = build_flow_run_started_event(
            "audit-20260901-101500",
            "alpha",
            None,
            "audit-routes",
            "",
            "/w/flow.js",
            "2026-09-01T10:15:00Z",
        );
        assert!(from_a_plain_shell.get("parent_sid").is_none());
    }

    #[test]
    fn flow_run_finished_names_a_brake_only_when_one_tripped() {
        let clean = build_flow_run_finished_event(
            "r1",
            "alpha",
            5,
            1.25,
            true,
            None,
            "2026-09-01T10:20:00Z",
        );
        assert_eq!(clean["agents"], 5);
        assert_eq!(clean["cost_usd"], 1.25);
        assert_eq!(clean["ok"], true);
        assert!(clean.get("brake").is_none());

        let braked = build_flow_run_finished_event(
            "r1",
            "alpha",
            5,
            1.25,
            false,
            Some("max_agents"),
            "2026-09-01T10:20:00Z",
        );
        assert_eq!(braked["ok"], false);
        assert_eq!(braked["brake"], "max_agents");
    }

    #[test]
    fn flow_free_text_cannot_widen_a_ledger_row_without_bound() {
        let event = build_flow_run_started_event(
            "r1",
            "alpha",
            None,
            &"n".repeat(500),
            &"d".repeat(5_000),
            &"p".repeat(2_000),
            "2026-09-01T10:15:00Z",
        );
        assert_eq!(event["name"].as_str().unwrap().chars().count(), 120);
        assert_eq!(event["description"].as_str().unwrap().chars().count(), 400);
        assert_eq!(event["script_path"].as_str().unwrap().chars().count(), 512);

        let brake = build_flow_brake_tripped_event("r1", "alpha", &"why ".repeat(500));
        assert_eq!(brake["reason"].as_str().unwrap().chars().count(), 256);
    }

    #[test]
    fn scheduled_event_never_carries_more_than_an_80_char_preview() {
        let event = build_scheduled_event(
            SCHEDULED_ENQUEUED,
            "d7",
            "s2",
            "2026-07-26T09:30:00Z",
            Some(&"x".repeat(100)),
            None,
        );
        assert_eq!(event["event"], SCHEDULED_ENQUEUED);
        assert_eq!(event["id"], "d7");
        assert_eq!(event["sid"], "s2");
        assert_eq!(event["preview"].as_str().unwrap().chars().count(), 80);
        assert!(event.get("text").is_none());
    }

    #[test]
    fn every_schema_kind_has_an_exhaustive_classification() {
        let mut wire_names = std::collections::HashSet::new();
        for &kind in EventKind::ALL {
            let _ = class(kind);
            assert!(wire_names.insert(kind.wire_name()));
            assert_eq!(EventKind::from_wire_name(kind.wire_name()), Some(kind));
        }
    }

    #[test]
    fn identical_latest_state_is_deduplicated_and_a_change_is_recovered() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let now = Instant::now();

        for sequence in 0..10_000 {
            let event = json!({
                "event": CODEX_RATE_LIMIT,
                "vendor": "codex",
                "snapshot": {"primary": {"usedPercent": 80}},
                "ts": format!("volatile-{sequence}"),
            });
            append_event_at(&path, &event, now, Some(Duration::ZERO)).unwrap();
        }
        assert_eq!(read_rows(&path).len(), 1);

        let changed = json!({
            "event": CODEX_RATE_LIMIT,
            "vendor": "codex",
            "snapshot": {"primary": {"usedPercent": 81}},
            "ts": "another-volatile-value",
        });
        append_event_at(&path, &changed, now, Some(Duration::ZERO)).unwrap();
        assert_eq!(read_rows(&path).len(), 2);

        let stats = kind_stats()
            .into_iter()
            .find(|stat| stat.kind == CODEX_RATE_LIMIT)
            .expect("rate-limit counters");
        assert!(stats.appended_count >= 2);
        assert!(stats.appended_bytes > 0);
        assert!(stats.suppressed_count >= 9_999);
        assert!(stats.suppressed_bytes > 0);
    }

    #[test]
    fn null_only_latest_state_is_never_persisted() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let event = json!({
            "event": CODEX_RATE_LIMIT,
            "vendor": "codex",
            "snapshot": {
                "primary": {"usedPercent": null, "resetsAt": null},
                "secondary": null,
            },
            "ts": "volatile",
        });

        append_event_at(&path, &event, Instant::now(), Some(Duration::ZERO)).unwrap();

        assert!(read_rows(&path).is_empty());
    }

    #[test]
    fn running_long_interval_is_scoped_per_sid() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let start = Instant::now();
        let running = |sid: &str, elapsed_sec: u64| {
            json!({
                "event": CHAT_TURN_RUNNING_LONG,
                "sid": sid,
                "slug": "demo",
                "turn_id": format!("turn-{sid}"),
                "elapsed_sec": elapsed_sec,
                "ts": format!("volatile-{elapsed_sec}"),
            })
        };

        append_event_at(&path, &running("s1", 300), start, None).unwrap();
        append_event_at(&path, &running("s2", 300), start, None).unwrap();
        append_event_at(
            &path,
            &running("s1", 599),
            start + Duration::from_secs(299),
            None,
        )
        .unwrap();
        append_event_at(
            &path,
            &running("s1", 600),
            start + Duration::from_secs(300),
            None,
        )
        .unwrap();

        let rows = read_rows(&path);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows.iter().filter(|row| row["sid"] == "s1").count(), 2);
        assert_eq!(rows.iter().filter(|row| row["sid"] == "s2").count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn latest_state_key_uses_the_canonical_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let real_dir = tmp.path().join("real");
        let alias_dir = tmp.path().join("alias");
        std::fs::create_dir(&real_dir).unwrap();
        std::os::unix::fs::symlink(&real_dir, &alias_dir).unwrap();
        let real_path = real_dir.join("progress.jsonl");
        let alias_path = alias_dir.join("progress.jsonl");
        let event = json!({
            "event": CODEX_THREAD_STATUS,
            "vendor": "codex",
            "thread_id": "thread-1",
            "status": "idle",
            "active_flags": [],
            "ts": "volatile",
        });

        append_event_at(&real_path, &event, Instant::now(), Some(Duration::ZERO)).unwrap();
        append_event_at(&alias_path, &event, Instant::now(), Some(Duration::ZERO)).unwrap();

        assert_eq!(read_rows(&real_path).len(), 1);
    }

    #[test]
    fn unknown_kind_is_persisted_as_a_counted_fact() {
        const UNKNOWN_FIXTURE: &str = "perf_v1_unknown_fixture";
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let before = kind_stats()
            .into_iter()
            .find(|stat| stat.kind == UNKNOWN_FIXTURE)
            .map(|stat| stat.appended_count)
            .unwrap_or_default();

        append_event(
            &path,
            &json!({"event": UNKNOWN_FIXTURE, "payload": "kept", "ts": "volatile"}),
        )
        .unwrap();

        assert_eq!(read_rows(&path).len(), 1);
        let after = kind_stats()
            .into_iter()
            .find(|stat| stat.kind == UNKNOWN_FIXTURE)
            .expect("unknown kind counter");
        assert!(after.unknown);
        assert_eq!(after.appended_count, before + 1);
        assert_eq!(after.suppressed_count, 0);
        assert!(after.appended_bytes > 0);
    }

    #[test]
    fn event_kind_extraction_prefers_event_then_falls_back_to_kind() {
        assert_eq!(
            event_kind_name(&json!({"kind": TYPED_EVENT})),
            Some(TYPED_EVENT)
        );
        assert_eq!(
            event_kind_name(&json!({"event": "legacy", "kind": TYPED_EVENT})),
            Some("legacy")
        );
        assert_eq!(
            event_kind_name(&json!({"event": null, "kind": MERGER_LOSSY_PARTIAL})),
            Some(MERGER_LOSSY_PARTIAL)
        );
    }
}
