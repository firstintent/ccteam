//! V0.2.2 F35 — event-aware silence classifier.
//!
//! `auto_loop::decide` only fires inside the Stop hook (input =
//! `last_assistant_text`). Two failure modes survive that path:
//!
//! - **API tool-call hang**: `PreToolUse` arrives but `PostToolUse` /
//!   `Stop` never does → auto-loop never triggers → the project sits
//!   idle forever on iteration 1.
//! - **send-keys routing miss** (F36 case): the orchestrator wrote a
//!   `phase_inject` event but the prompt landed in a sub-agent's
//!   context → no `Stop` → auto-loop never triggers.
//!
//! `stall.rs` already converts "silent for N seconds" into 5/15/30 min
//! soft warnings, but those are **observation**-only — they never
//! drive a recovery action. F35 reads progress.jsonl's tail event +
//! the silent-second budget, classifies the project into one of seven
//! buckets, and tells `orchestrator::poll_tick` whether to surface an
//! enriched `needs_attention.outbox.json` payload or fire a single
//! deterministic re-injection.
//!
//! **Red lines** (CLAUDE.md §三):
//!
//! - **No LLM**: `classify` is pure deterministic match — no
//!   `claude -p` / model call.
//! - **No active kill**: F35 never sends `Ctrl-C` or kills tmux. Limbo
//!   classes ask the orchestrator to re-inject the current phase
//!   prompt (deterministic; reuses the existing send-keys path); when
//!   the per-phase retry cap (`MAX_LIMBO_RETRY = 1`) is exhausted, we
//!   fall through to the same enriched-outbox surface that hung-tool
//!   classes use. V0.3 may evaluate Ctrl-C; F35 doesn't.
//! - **Pane tail never re-enters the state machine**: the captured
//!   `tmux capture-pane` text rides the outbox payload as
//!   user-readable detail; the classifier consumes
//!   `progress.jsonl` events only.
//! - **Meta-agent / evergreen sessions skip classification**:
//!   `orchestrator::poll_tick` calls `classify` only for non-evergreen
//!   teams (the route already filters via `is_evergreen()`).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::stall::StallThresholds;

/// How many deterministic re-injects F35 can attempt per phase before
/// it stops trying and routes the case to the enriched-outbox surface.
/// **1**: matches the briefing red-line "fix-loop 撞 3 次顶必 escalate,
/// 绝不静默重置" — the underlying `auto_loop` keeps its 3-cap; F35 only
/// adds **one** extra deterministic re-inject for limbo cases the Stop
/// hook can't even see.
pub const MAX_LIMBO_RETRY: u32 = 1;

/// Filename under `<project>/.ccteam/` where the per-phase F35 retry
/// counter lives. One file per project; reset on phase advance so a
/// later phase's first limbo gets its own retry budget.
pub const LIMBO_RETRY_FILE: &str = "limbo-retry-count.json";

/// Seven-class silence taxonomy. `Healthy` / `Terminal` need no action;
/// `SubagentBusy` is "patient, do nothing — wait it out"; the four
/// remaining variants drive concrete orchestrator side-effects (see
/// `LimboAction::from`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SilenceClass {
    /// Last event is `PostToolUse` / `phase_done` / `escalate` /
    /// `phase_inject` (recently dispatched) and silence is below the
    /// warn threshold. No action.
    Healthy,
    /// Last event is `phase_done` or `escalate` — the project is at
    /// rest by design. No action.
    Terminal,
    /// `PreToolUse(tool=Task)` was the most recent activity and a
    /// matching `SubagentStop` hasn't arrived yet, but silence is
    /// still under the escalate threshold. Sub-agents legitimately
    /// run for many minutes — this is the "patient" bucket.
    SubagentBusy,
    /// Same shape as `SubagentBusy` but silence ≥ escalate threshold —
    /// the sub-agent is genuinely stuck or detached. Surface as
    /// enriched escalate.
    SubagentRunaway,
    /// `PreToolUse(tool != Task)` was the most recent event and the
    /// matching `PostToolUse` never arrived; silence ≥ warn threshold.
    /// API/tool hang. Surface as enriched escalate (no auto re-inject —
    /// the tool call would just re-hang).
    /// Carries the tool name so the outbox payload can name it.
    MidToolHung(String),
    /// `Stop` / `SubagentStop` was last and silence ≥ warn threshold;
    /// the auto-loop's `iteration` counter hasn't advanced (Stop hook
    /// either never observed completion or its decision didn't
    /// land). Deterministic re-inject 1 ×, then enriched escalate.
    PostStopLimbo,
    /// `phase_inject` was last, silence ≥ warn threshold, and **no**
    /// downstream events landed (no PreToolUse, no Stop). The send-keys
    /// likely went to a sub-agent's context (F36). Deterministic
    /// re-inject 1 ×, then enriched escalate.
    InjectLimbo,
}

impl SilenceClass {
    /// Stable string form for outbox payloads / log lines. Variants
    /// with payloads (`MidToolHung`) collapse to the discriminant —
    /// the payload travels as a separate outbox field.
    pub fn discriminant(&self) -> &'static str {
        match self {
            SilenceClass::Healthy => "healthy",
            SilenceClass::Terminal => "terminal",
            SilenceClass::SubagentBusy => "subagent_busy",
            SilenceClass::SubagentRunaway => "subagent_runaway",
            SilenceClass::MidToolHung(_) => "mid_tool_hung",
            SilenceClass::PostStopLimbo => "post_stop_limbo",
            SilenceClass::InjectLimbo => "inject_limbo",
        }
    }
}

/// What the orchestrator should do with a `SilenceClass`. Pure
/// translation — `LimboAction` doesn't know how to write outbox files
/// or send keys; `poll_tick` does. Splitting keeps `classify` pure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LimboAction {
    /// No action needed.
    NoOp,
    /// Surface an enriched `needs_attention.outbox.json` payload with
    /// `class` / `silent_seconds` / `last_event` / `pane_tail`.
    EnrichedEscalate,
    /// Issue **one** deterministic re-inject of the current phase
    /// prompt (orchestrator owns the send-keys path). When the
    /// per-phase retry cap is reached `LimboAction::EnrichedEscalate`
    /// is returned instead — that decision lives in
    /// `orchestrator::poll_tick`.
    DeterministicReinject,
}

impl LimboAction {
    /// Map a class to the action `poll_tick` should perform **before**
    /// consulting the per-phase retry counter. `poll_tick` then
    /// downgrades `DeterministicReinject` to `EnrichedEscalate` if
    /// the cap is exhausted.
    pub fn from(class: &SilenceClass) -> Self {
        match class {
            SilenceClass::Healthy
            | SilenceClass::Terminal
            | SilenceClass::SubagentBusy => LimboAction::NoOp,
            SilenceClass::SubagentRunaway | SilenceClass::MidToolHung(_) => {
                LimboAction::EnrichedEscalate
            }
            SilenceClass::PostStopLimbo | SilenceClass::InjectLimbo => {
                LimboAction::DeterministicReinject
            }
        }
    }
}

/// Pure classifier. **No I/O, no LLM call.** The orchestrator is
/// responsible for reading `progress.jsonl` and computing
/// `silent_seconds` before invoking this.
///
/// Decision order (matches PRD §4.2.1 table):
///
/// 1. Empty event log → `Healthy` (project never started).
/// 2. Tail event is `phase_done` / `escalate` → `Terminal`.
/// 3. Tail event is `PreToolUse` with `tool == "Task"`:
///    - silence ≥ escalate → `SubagentRunaway`
///    - silence < escalate → `SubagentBusy`
/// 4. Tail event is `PreToolUse` (any other tool) and silence ≥ warn
///    → `MidToolHung(<tool>)`.
/// 5. Tail event is `Stop` / `SubagentStop` and silence ≥ warn →
///    `PostStopLimbo`.
/// 6. Tail event is `phase_inject` and silence ≥ warn → `InjectLimbo`
///    (no PreToolUse / Stop has landed since the inject — F36's case).
/// 7. Anything else → `Healthy`.
pub fn classify(
    events: &[Value],
    silent_seconds: u64,
    thresholds: &StallThresholds,
) -> SilenceClass {
    let Some(last) = events.iter().rev().find(|e| !is_skipped_event(e)) else {
        return SilenceClass::Healthy;
    };
    let kind = event_kind(last);
    let warn = silent_seconds >= thresholds.warn_seconds;
    let escalate = silent_seconds >= thresholds.escalate_seconds;
    match kind {
        "phase_done" | "escalate" => SilenceClass::Terminal,
        "PreToolUse" => {
            let tool = event_tool(last).unwrap_or_default();
            if tool == "Task" {
                if escalate {
                    SilenceClass::SubagentRunaway
                } else {
                    SilenceClass::SubagentBusy
                }
            } else if warn {
                SilenceClass::MidToolHung(tool.to_string())
            } else {
                SilenceClass::Healthy
            }
        }
        "Stop" | "SubagentStop" => {
            if warn {
                SilenceClass::PostStopLimbo
            } else {
                SilenceClass::Healthy
            }
        }
        "phase_inject" => {
            if warn {
                SilenceClass::InjectLimbo
            } else {
                SilenceClass::Healthy
            }
        }
        _ => SilenceClass::Healthy,
    }
}

/// Skip transient meta events that don't reflect "agent activity"
/// when picking the tail event. `inbox_consumed` / `golden_rules_check`
/// fire alongside the genuine business event and would otherwise mask
/// it (the classifier wants the latest agent / orchestrator action).
fn is_skipped_event(event: &Value) -> bool {
    matches!(
        event_kind(event),
        "inbox_consumed" | "golden_rules_check" | "session_start" | "SessionEnd",
    )
}

fn event_kind(event: &Value) -> &str {
    event.get("event").and_then(|s| s.as_str()).unwrap_or("")
}

fn event_tool(event: &Value) -> Option<&str> {
    event.get("tool").and_then(|s| s.as_str())
}

/// Compact summary of the tail event for the outbox payload — keeps
/// the user-facing JSON readable without surfacing full progress event
/// shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LastEventSummary {
    /// RFC3339 timestamp from the original event (best-effort — `""` if
    /// the source row had no `ts`).
    pub ts: String,
    pub event: String,
    /// `tool` is only present on `PreToolUse` / `PostToolUse` events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
}

impl LastEventSummary {
    pub fn from_value(event: &Value) -> Self {
        Self {
            ts: event
                .get("ts")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string(),
            event: event_kind(event).to_string(),
            tool: event_tool(event).map(str::to_string),
        }
    }
}

/// Per-phase F35 retry counter. One file per project; the on-disk
/// shape is `{ "phase": <name>, "count": <u32>, "last_at": <RFC3339> }`.
/// Phase advance resets it (the orchestrator calls `reset_for_phase`
/// when `current_phase` changes).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LimboRetryCount {
    pub phase: String,
    pub count: u32,
    #[serde(default)]
    pub last_at: Option<DateTime<Utc>>,
}

impl LimboRetryCount {
    pub fn fresh(phase: impl Into<String>) -> Self {
        Self {
            phase: phase.into(),
            count: 0,
            last_at: None,
        }
    }
}

/// `<project>/.ccteam/limbo-retry-count.json`.
pub fn retry_path_in(project_dir: &Path) -> PathBuf {
    project_dir.join(".ccteam").join(LIMBO_RETRY_FILE)
}

/// Load the per-phase retry counter. `phase_mismatch` returns a fresh
/// counter (i.e. previous phase's budget doesn't roll over). Missing
/// file ⇒ fresh counter. Parse failures fail-loud so a corrupt counter
/// is visible at the next tick instead of silently resetting.
pub fn load_retry_count(path: &Path, current_phase: &str) -> Result<LimboRetryCount> {
    if !path.exists() {
        return Ok(LimboRetryCount::fresh(current_phase));
    }
    let body = std::fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?;
    if body.trim().is_empty() {
        return Ok(LimboRetryCount::fresh(current_phase));
    }
    let counter: LimboRetryCount = serde_json::from_str(&body)
        .with_context(|| format!("parse {}", path.display()))?;
    if counter.phase != current_phase {
        Ok(LimboRetryCount::fresh(current_phase))
    } else {
        Ok(counter)
    }
}

/// Persist the retry counter atomically (`<path>.tmp` then `rename`).
pub fn save_retry_count(path: &Path, counter: &LimboRetryCount) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create {}", parent.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(counter)
        .context("serialize limbo-retry-count")?;
    std::fs::write(&tmp, body).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Reset the counter when a phase advances. Equivalent to
/// `save_retry_count(path, &LimboRetryCount::fresh(phase))` but with a
/// dedicated name for callers that don't need the current value.
pub fn reset_retry_count(path: &Path, phase: &str) -> Result<()> {
    save_retry_count(path, &LimboRetryCount::fresh(phase))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn pretool(tool: &str) -> Value {
        json!({"ts": "2026-05-09T10:00:00Z", "event": "PreToolUse", "tool": tool})
    }
    fn stop() -> Value {
        json!({"ts": "2026-05-09T10:00:00Z", "event": "Stop"})
    }
    fn subagent_stop() -> Value {
        json!({"ts": "2026-05-09T10:00:00Z", "event": "SubagentStop"})
    }
    fn phase_inject() -> Value {
        json!({"ts": "2026-05-09T10:00:00Z", "event": "phase_inject", "phase": "implement"})
    }
    fn phase_done() -> Value {
        json!({"ts": "2026-05-09T10:00:00Z", "event": "phase_done", "phase": "implement"})
    }
    fn posttool() -> Value {
        json!({"ts": "2026-05-09T10:00:00Z", "event": "PostToolUse", "tool": "Read"})
    }

    fn defaults() -> StallThresholds {
        StallThresholds::default()
    }

    #[test]
    fn classify_empty_log_is_healthy() {
        assert_eq!(classify(&[], 0, &defaults()), SilenceClass::Healthy);
        assert_eq!(
            classify(&[], 99 * 60, &defaults()),
            SilenceClass::Healthy,
            "no events == project never started; silence doesn't promote",
        );
    }

    #[test]
    fn classify_terminal_states() {
        assert_eq!(
            classify(&[phase_done()], 99 * 60, &defaults()),
            SilenceClass::Terminal,
        );
        let escalate = json!({"ts": "...", "event": "escalate"});
        assert_eq!(
            classify(&[escalate], 99 * 60, &defaults()),
            SilenceClass::Terminal,
        );
    }

    #[test]
    fn classify_subagent_busy_below_escalate_threshold() {
        // Default thresholds: warn 5min, escalate 30min. 10 minutes
        // is past warn but well below escalate — sub-agents commonly
        // run that long, must stay patient.
        let events = [pretool("Task")];
        assert_eq!(
            classify(&events, 10 * 60, &defaults()),
            SilenceClass::SubagentBusy,
        );
    }

    #[test]
    fn classify_subagent_runaway_at_escalate_threshold() {
        let events = [pretool("Task")];
        assert_eq!(
            classify(&events, 30 * 60, &defaults()),
            SilenceClass::SubagentRunaway,
        );
        assert_eq!(
            classify(&events, 60 * 60, &defaults()),
            SilenceClass::SubagentRunaway,
        );
    }

    #[test]
    fn classify_mid_tool_hung_carries_tool_name() {
        let events = [pretool("Read")];
        match classify(&events, 6 * 60, &defaults()) {
            SilenceClass::MidToolHung(tool) => assert_eq!(tool, "Read"),
            other => panic!("expected MidToolHung(Read), got {other:?}"),
        }
    }

    #[test]
    fn classify_mid_tool_below_warn_is_healthy() {
        let events = [pretool("Read")];
        // 4 min < default warn 5 min — tool calls < 5 min are normal.
        assert_eq!(classify(&events, 4 * 60, &defaults()), SilenceClass::Healthy);
    }

    #[test]
    fn classify_post_stop_limbo_after_warn_threshold() {
        assert_eq!(
            classify(&[stop()], 6 * 60, &defaults()),
            SilenceClass::PostStopLimbo,
        );
        assert_eq!(
            classify(&[subagent_stop()], 6 * 60, &defaults()),
            SilenceClass::PostStopLimbo,
        );
    }

    #[test]
    fn classify_post_stop_below_warn_is_healthy() {
        assert_eq!(
            classify(&[stop()], 60, &defaults()),
            SilenceClass::Healthy,
            "Stop within the first 5 minutes is just an idle pause",
        );
    }

    #[test]
    fn classify_inject_limbo_after_warn_threshold() {
        assert_eq!(
            classify(&[phase_inject()], 6 * 60, &defaults()),
            SilenceClass::InjectLimbo,
        );
    }

    #[test]
    fn classify_inject_below_warn_is_healthy() {
        // Just dispatched — give the assistant time to act before
        // declaring limbo.
        assert_eq!(
            classify(&[phase_inject()], 60, &defaults()),
            SilenceClass::Healthy,
        );
    }

    #[test]
    fn classify_skips_meta_events_for_tail_lookup() {
        // The orchestrator may emit `inbox_consumed` after a Stop
        // event; the classifier wants the underlying agent activity,
        // not the meta event.
        let inbox = json!({"ts": "...", "event": "inbox_consumed", "session": "x"});
        let events = [pretool("Task"), inbox];
        assert_eq!(
            classify(&events, 10 * 60, &defaults()),
            SilenceClass::SubagentBusy,
        );
    }

    #[test]
    fn classify_post_tool_use_is_healthy() {
        // PostToolUse means the tool returned — the agent is alive.
        assert_eq!(
            classify(&[posttool()], 60, &defaults()),
            SilenceClass::Healthy,
        );
    }

    #[test]
    fn classify_phase_thresholds_respected() {
        // `04-primary` style phase with stall_warn_minutes: 60 →
        // warn=60min, escalate=360min. 30 minutes of silence on a
        // PreToolUse(Task) must NOT escalate.
        let t = StallThresholds::from_phase(Some(60));
        let events = [pretool("Task")];
        assert_eq!(
            classify(&events, 30 * 60, &t),
            SilenceClass::SubagentBusy,
            "30 min < 60 min warn threshold for long-running phases",
        );
        assert_eq!(
            classify(&events, 360 * 60, &t),
            SilenceClass::SubagentRunaway,
        );
    }

    #[test]
    fn limbo_action_maps_classes_to_orchestrator_intent() {
        assert_eq!(
            LimboAction::from(&SilenceClass::Healthy),
            LimboAction::NoOp,
        );
        assert_eq!(
            LimboAction::from(&SilenceClass::Terminal),
            LimboAction::NoOp,
        );
        assert_eq!(
            LimboAction::from(&SilenceClass::SubagentBusy),
            LimboAction::NoOp,
        );
        assert_eq!(
            LimboAction::from(&SilenceClass::SubagentRunaway),
            LimboAction::EnrichedEscalate,
        );
        assert_eq!(
            LimboAction::from(&SilenceClass::MidToolHung("Read".into())),
            LimboAction::EnrichedEscalate,
        );
        assert_eq!(
            LimboAction::from(&SilenceClass::PostStopLimbo),
            LimboAction::DeterministicReinject,
        );
        assert_eq!(
            LimboAction::from(&SilenceClass::InjectLimbo),
            LimboAction::DeterministicReinject,
        );
    }

    #[test]
    fn discriminant_strings_are_stable() {
        assert_eq!(SilenceClass::Healthy.discriminant(), "healthy");
        assert_eq!(SilenceClass::Terminal.discriminant(), "terminal");
        assert_eq!(SilenceClass::SubagentBusy.discriminant(), "subagent_busy");
        assert_eq!(
            SilenceClass::SubagentRunaway.discriminant(),
            "subagent_runaway",
        );
        assert_eq!(
            SilenceClass::MidToolHung("X".into()).discriminant(),
            "mid_tool_hung",
        );
        assert_eq!(
            SilenceClass::PostStopLimbo.discriminant(),
            "post_stop_limbo",
        );
        assert_eq!(SilenceClass::InjectLimbo.discriminant(), "inject_limbo");
    }

    #[test]
    fn last_event_summary_extracts_fields() {
        let s = LastEventSummary::from_value(&pretool("Read"));
        assert_eq!(s.event, "PreToolUse");
        assert_eq!(s.tool.as_deref(), Some("Read"));
        assert_eq!(s.ts, "2026-05-09T10:00:00Z");
    }

    #[test]
    fn last_event_summary_handles_missing_tool() {
        let s = LastEventSummary::from_value(&stop());
        assert_eq!(s.event, "Stop");
        assert!(s.tool.is_none());
    }

    #[test]
    fn retry_count_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("limbo-retry-count.json");
        let mut counter = LimboRetryCount::fresh("implement");
        counter.count = 1;
        counter.last_at = Some(Utc::now());
        save_retry_count(&path, &counter).unwrap();
        let loaded = load_retry_count(&path, "implement").unwrap();
        assert_eq!(loaded.phase, "implement");
        assert_eq!(loaded.count, 1);
    }

    #[test]
    fn retry_count_resets_on_phase_change() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("limbo-retry-count.json");
        let mut counter = LimboRetryCount::fresh("implement");
        counter.count = 1;
        save_retry_count(&path, &counter).unwrap();
        // Phase advanced — old counter must not carry over.
        let loaded = load_retry_count(&path, "review").unwrap();
        assert_eq!(loaded.phase, "review");
        assert_eq!(loaded.count, 0);
    }

    #[test]
    fn retry_count_missing_file_returns_fresh() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("nope.json");
        let loaded = load_retry_count(&path, "any").unwrap();
        assert_eq!(loaded.count, 0);
        assert_eq!(loaded.phase, "any");
    }

    #[test]
    fn max_limbo_retry_is_one() {
        // Red-line: F35 only adds **one** deterministic re-inject on
        // top of the underlying auto_loop's 3-cap. Any change to this
        // constant breaks the "fix-loop 撞 3 次顶必 escalate" red line
        // — surface the breakage as a failing test.
        assert_eq!(MAX_LIMBO_RETRY, 1);
    }
}
