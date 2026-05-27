//! progress.jsonl reader / writer + idle detection + workflow event aggregations.
//!
//! progress.jsonl is the orchestrator's only state-truth source
//! (`docs/tech-design.md` §5.5). This module gives both the hook
//! handlers and the orchestrator a single set of primitives so the
//! file format and idle semantics stay in sync.
//!
//! V0.4.0 F60: the phase-prompt builders (`build_phase_prompt`,
//! `build_phase_prompt_for_template`, `build_phase_prompt_with_attachments`,
//! `build_phase_prompt_for_template_with_team`) were deleted along
//! with the rest of the phase machinery. F66 reintroduces an
//! injection-prompt builder against the new `workflow.yaml` schema.
//! Event-log read/write/idle helpers stay — they're channel-layer
//! primitives shared by every consumer.
//!
//! V0.4.0 F67: workflow-event aggregation helpers
//! (`workflow_cost_total`, `current_agent_sessions`,
//! `escalation_count`) read F66's 8 canonical event kinds
//! (`workflow_start` / `agent_spawn` / `agent_done` /
//! `artifact_received` / `gate_triggered` / `budget_exceeded` /
//! `workflow_done` / `escalation`). They are pure functions over a
//! `&[Value]` slice — no IO, no state — so call sites can choose how
//! to source the slice (one-shot read, tail follow, etc.).

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

/// Append `event` as one JSONL line. Creates parent dir + file when
/// missing. POSIX `O_APPEND` is atomic for sub-PIPE_BUF writes (4 KiB
/// on Linux), which our compact event lines comfortably fit under, so
/// concurrent hook + orchestrator writers don't interleave.
pub fn append_event(path: &Path, event: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let line = serde_json::to_string(event)? + "\n";
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    f.write_all(line.as_bytes())
        .with_context(|| format!("append to {}", path.display()))?;
    Ok(())
}

/// Read + parse the last non-empty line of `path`. `Ok(None)` when the
/// file is absent or contains no events yet.
pub fn last_event(path: &Path) -> Result<Option<Value>> {
    if !path.exists() {
        return Ok(None);
    }
    let content =
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let Some(line) = content.lines().rev().find(|l| !l.trim().is_empty()) else {
        return Ok(None);
    };
    let v: Value = serde_json::from_str(line.trim())
        .with_context(|| format!("parse last line of {}", path.display()))?;
    Ok(Some(v))
}

/// Read + parse all events from `path`. Skips empty lines and lines
/// that fail to deserialize as JSON (defensive: a half-flushed line
/// shouldn't crash the orchestrator's read).
pub fn read_all_events(path: &Path) -> Result<Vec<Value>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content =
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let mut out = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(trimmed) {
            Ok(v) => out.push(v),
            Err(_) => continue,
        }
    }
    Ok(out)
}

/// Idle detection per tech-design §6.9.
///
/// `Stop` / `Notification:idle_prompt` are the canonical "claude is
/// waiting" signals. Phase-boundary events (`session_start`,
/// `phase_done`, `escalate`, `SessionEnd`) also imply nothing is
/// in-flight. `SubagentStop` fires 2–5 s after `Stop` whenever the
/// finished turn used `Task`; the main loop is already idle by then,
/// so we treat it the same as `Stop` (E2E 2026-05-06: classifying it
/// as busy caused the next phase prompt to be wrapped in `/btw`,
/// which spawns a tool-less side-agent and stalls the project).
/// Anything else (`PreToolUse`, `PostToolUse`, `phase_inject`) means a
/// tool call is mid-flight — caller should use `/btw` to queue without
/// interrupting.
pub fn is_idle(last: Option<&Value>) -> bool {
    let Some(event) = last else {
        return true;
    };
    let kind = event.get("event").and_then(|s| s.as_str()).unwrap_or("");
    matches!(
        kind,
        "Stop"
            | "SubagentStop"
            | "notification"
            | "session_start"
            | "SessionEnd"
            | "phase_done"
            | "escalate"
            // V0.6.0 F108 — chat-mode terminal boundaries. After a turn
            // completes / session resets / compaction lands, the TUI
            // session is waiting for the next user input → idle.
            | CHAT_TURN_COMPLETED
            | CHAT_SESSION_STARTED
            | CHAT_SESSION_RESET
            | CHAT_SESSION_RESET_WITH_RECOVERY
            | CHAT_COMPACT_DONE
    )
}

/// V0.2.2 F36: detect whether a sub-agent (`Task` tool) is currently
/// in flight by walking `events` from the tail and counting how many
/// `PreToolUse(tool=Task)` openings have not yet been matched by a
/// `SubagentStop`. Returns `true` when at least one window is open.
///
/// **Why count, not last-event-match**: Claude Code can launch a
/// sub-agent (`Task`), have it spawn an inner Task, and emit two
/// `PreToolUse(Task)` events in a row before the matching pair of
/// `SubagentStop` events arrives. A naive "is the most recent event a
/// `Task` PreToolUse?" check misses the second-from-top case the
/// moment the inner sub-agent emits its own `PreToolUse`.
///
/// **Why scan from the tail**: every `SubagentStop` past the open
/// window already cancelled an earlier `PreToolUse(Task)` we don't
/// care about. We stop counting as soon as `open_windows` returns to
/// zero — older paired sequences can't reach into the current open
/// state.
///
/// Pure deterministic helper; no I/O. Honors the **"`progress.jsonl`
/// is the only state truth"** red line — F36's send-keys guard reads
/// progress events, never tmux pane text.
pub fn subagent_active(events: &[Value]) -> bool {
    let mut closes_pending: u64 = 0;
    for event in events.iter().rev() {
        let kind = event.get("event").and_then(|s| s.as_str()).unwrap_or("");
        match kind {
            "SubagentStop" => {
                closes_pending = closes_pending.saturating_add(1);
            }
            "PreToolUse" => {
                let tool = event.get("tool").and_then(|s| s.as_str()).unwrap_or("");
                if tool == "Task" {
                    if closes_pending == 0 {
                        return true;
                    }
                    closes_pending -= 1;
                }
            }
            _ => {}
        }
    }
    false
}

/// `/btw <prompt>` when claude is busy so the message queues without
/// interrupting; bare prompt when idle.
pub fn idle_aware_message(prompt: &str, idle: bool) -> String {
    if idle {
        prompt.to_string()
    } else {
        format!("/btw {prompt}")
    }
}

// ---------------- V0.6.0 F108 / F118 chat-mode event kinds ----------------

/// `chat_session_started` — Claude Code TUI session has spawned (tmux up,
/// SessionStart hook fired). Payload: `{role, project_dir, ts}`.
pub const CHAT_SESSION_STARTED: &str = "chat_session_started";

/// `chat_turn_user_prompt` — user submitted a turn (UserPromptSubmit hook).
/// Payload: `{role, prompt_excerpt, turn_id, ts}`.
pub const CHAT_TURN_USER_PROMPT: &str = "chat_turn_user_prompt";

/// `chat_turn_completed` — assistant turn finished (Stop hook). Payload:
/// `{role, turn_id, usage: UnifiedTokenUsage, ts}`.
pub const CHAT_TURN_COMPLETED: &str = "chat_turn_completed";

/// `chat_session_reset` — user / orchestrator issued `/clear` or `/new`
/// inside the TUI session. Payload: `{role, ts}`.
pub const CHAT_SESSION_RESET: &str = "chat_session_reset";

/// `chat_session_reset_with_recovery` — F118: session-id was invalidated
/// (compaction failure, transcript corruption, manual rebuild) and the
/// orchestrator rehydrated last-N turns from `<bot>/turns.jsonl` into a
/// fresh tmux session. Payload: `{role, recovered_turns, ts}`.
pub const CHAT_SESSION_RESET_WITH_RECOVERY: &str = "chat_session_reset_with_recovery";

/// `chat_compact_done` — Claude Code finished a `/compact` (PreCompact +
/// PostCompact hooks bracket the operation). Payload: `{role, ts}`.
pub const CHAT_COMPACT_DONE: &str = "chat_compact_done";

/// `chat_hop_escalate` — bot consulted another bot via `@<handle>` >=
/// `hop_limit` times; orchestrator emits this so the meta-agent / UI can
/// surface the hop-loop. Payload: `{role, hop_count, last_bot, ts}`.
pub const CHAT_HOP_ESCALATE: &str = "chat_hop_escalate";

/// Build a `chat_session_started` event JSON. `role` is the bot handle
/// (`workflow.yaml mode: chat` `bot_name`); `project_dir` is the
/// ccteam-managed project root.
pub fn build_chat_session_started_event(role: &str, project_dir: &str) -> Value {
    serde_json::json!({
        "event": CHAT_SESSION_STARTED,
        "role": role,
        "project_dir": project_dir,
        "ts": Utc::now().to_rfc3339(),
    })
}

/// Build a `chat_turn_user_prompt` event JSON. `prompt_excerpt` should
/// be a short (<= 256 char) summary to keep progress.jsonl scannable —
/// the full prompt lives in the bot's `turns.jsonl` mirror.
pub fn build_chat_turn_user_prompt_event(role: &str, turn_id: &str, prompt_excerpt: &str) -> Value {
    let trimmed: String = prompt_excerpt.chars().take(256).collect();
    serde_json::json!({
        "event": CHAT_TURN_USER_PROMPT,
        "role": role,
        "turn_id": turn_id,
        "prompt_excerpt": trimmed,
        "ts": Utc::now().to_rfc3339(),
    })
}

/// Build a `chat_turn_completed` event JSON. The `usage` field uses
/// `serde_json::to_value(UnifiedTokenUsage)` so the wire shape matches
/// the rest of the cost pipeline.
pub fn build_chat_turn_completed_event(
    role: &str,
    turn_id: &str,
    usage: &crate::harness::UnifiedTokenUsage,
) -> Value {
    serde_json::json!({
        "event": CHAT_TURN_COMPLETED,
        "role": role,
        "turn_id": turn_id,
        "usage": serde_json::to_value(usage).unwrap_or(Value::Null),
        "ts": Utc::now().to_rfc3339(),
    })
}

/// Build a `chat_session_reset` event JSON.
pub fn build_chat_session_reset_event(role: &str) -> Value {
    serde_json::json!({
        "event": CHAT_SESSION_RESET,
        "role": role,
        "ts": Utc::now().to_rfc3339(),
    })
}

/// V0.6.6 F172 V2 — build a `chat_session_reset` event with an explicit
/// `reason` field so user-visible context loss (e.g. failed
/// `claude --resume <name>` fallback to fresh) carries enough metadata
/// for IM / web surfaces to distinguish from user-issued `/clear`.
pub fn build_chat_session_reset_event_with_reason(role: &str, reason: &str) -> Value {
    serde_json::json!({
        "event": CHAT_SESSION_RESET,
        "role": role,
        "reason": reason,
        "ts": Utc::now().to_rfc3339(),
    })
}

/// Build a `chat_session_reset_with_recovery` event JSON (F118).
pub fn build_chat_session_reset_with_recovery_event(role: &str, recovered_turns: usize) -> Value {
    serde_json::json!({
        "event": CHAT_SESSION_RESET_WITH_RECOVERY,
        "role": role,
        "recovered_turns": recovered_turns,
        "ts": Utc::now().to_rfc3339(),
    })
}

/// Build a `chat_compact_done` event JSON.
pub fn build_chat_compact_done_event(role: &str) -> Value {
    serde_json::json!({
        "event": CHAT_COMPACT_DONE,
        "role": role,
        "ts": Utc::now().to_rfc3339(),
    })
}

/// Build a `chat_hop_escalate` event JSON.
pub fn build_chat_hop_escalate_event(role: &str, hop_count: u32, last_bot: &str) -> Value {
    serde_json::json!({
        "event": CHAT_HOP_ESCALATE,
        "role": role,
        "hop_count": hop_count,
        "last_bot": last_bot,
        "ts": Utc::now().to_rfc3339(),
    })
}

/// `chat_bot_permanent_failure` — V0.6.8 F192c. Emitted after a bot's
/// `HarnessAdapter::start_thread` has failed `MAX_START_THREAD_ATTEMPTS`
/// times in a row. The supervisor latches `permanent_failure = true`,
/// stops retrying, and the daemon's tick loop returns `Quarantine` on
/// every subsequent decision pass for this bot. Recovery requires
/// `ccteam restart-bot <slug>/<role>` or a daemon restart.
///
/// Payload: `{role, reason, attempts, ts}`.
pub const CHAT_BOT_PERMANENT_FAILURE: &str = "chat_bot_permanent_failure";

/// V0.6.8 F192c — build a `chat_bot_permanent_failure` event JSON.
/// `reason` is a short human string summarizing the latest failure
/// (typically the underlying tmux / spawn stderr, truncated to keep
/// progress.jsonl scannable). `attempts` is the number of consecutive
/// failed start_thread attempts (always 3 at the moment but kept as a
/// field so future tuning of `MAX_START_THREAD_ATTEMPTS` lands on the
/// wire without a schema bump).
pub fn build_chat_bot_permanent_failure_event(role: &str, reason: &str, attempts: u32) -> Value {
    let trimmed: String = reason.chars().take(512).collect();
    serde_json::json!({
        "event": CHAT_BOT_PERMANENT_FAILURE,
        "role": role,
        "reason": trimmed,
        "attempts": attempts,
        "ts": Utc::now().to_rfc3339(),
    })
}

/// `chat_marker_self_heal_attempt` — V0.6.8 F196. Emitted each time the
/// supervisor escalates from a sustained "marker missing" state to a
/// session reset. The SessionStart hook writes the F176
/// `active-session-id` marker; when it fails (state.json missing,
/// hook env propagation broke, hook subprocess errored), the tail loop
/// polls forever and the bot is silently dead despite a healthy tmux
/// pane. After [`MARKER_MISSING_RESET_THRESHOLD`] consecutive
/// marker-missing reports the supervisor calls `reset_session` to
/// auto-recover (same code path operators trigger via
/// `signals/reset.signal`). This event records the escalation for
/// operator-facing observability.
///
/// Payload: `{role, attempt_n, ts}`. `attempt_n` is the 1-based index
/// of the heal attempt (1..=`MAX_MARKER_SELF_HEAL_ATTEMPTS`).
pub const CHAT_MARKER_SELF_HEAL_ATTEMPT: &str = "chat_marker_self_heal_attempt";

/// V0.6.8 F196 — build a `chat_marker_self_heal_attempt` event JSON.
/// See [`CHAT_MARKER_SELF_HEAL_ATTEMPT`] for semantics.
pub fn build_chat_marker_self_heal_attempt_event(role: &str, attempt_n: u32) -> Value {
    serde_json::json!({
        "event": CHAT_MARKER_SELF_HEAL_ATTEMPT,
        "role": role,
        "attempt_n": attempt_n,
        "ts": Utc::now().to_rfc3339(),
    })
}

/// `chat_bot_marker_stuck` — V0.6.8 F196. Emitted after the supervisor
/// has burned through `MAX_MARKER_SELF_HEAL_ATTEMPTS` consecutive
/// session resets without the F176 `active-session-id` marker ever
/// appearing again. At this point the supervisor latches the
/// "marker stuck" state and stops attempting further self-heal resets
/// (same envelope as F192c's `chat_bot_permanent_failure`). Recovery:
/// operator restores the SessionStart hook prerequisite (typically
/// re-creates the bot's `state.json`) and runs
/// `ccteam restart-bot <slug>/<role>` or writes `signals/reset.signal`.
///
/// Payload: `{role, attempts, ts}`. `attempts` is the number of
/// consecutive failed self-heal resets, always
/// `MAX_MARKER_SELF_HEAL_ATTEMPTS` at the moment but kept as a field so
/// future tuning lands on the wire without a schema bump.
pub const CHAT_BOT_MARKER_STUCK: &str = "chat_bot_marker_stuck";

/// V0.6.8 F196 — build a `chat_bot_marker_stuck` event JSON.
/// See [`CHAT_BOT_MARKER_STUCK`] for semantics.
pub fn build_chat_bot_marker_stuck_event(role: &str, attempts: u32) -> Value {
    serde_json::json!({
        "event": CHAT_BOT_MARKER_STUCK,
        "role": role,
        "attempts": attempts,
        "ts": Utc::now().to_rfc3339(),
    })
}

/// `chat_turn_running_long` — V0.6.8 F195. Per-turn watchdog crossed
/// the first threshold (`1× turn_timeout_sec`, default 90s). The
/// supervisor is **still** waiting on `chat_turn_completed`; this event
/// signals the wait crossed the warning bar so IM / web surfaces can
/// reassure the user the bot is alive. Hard rule: **does not kill the
/// turn** — the underlying claude session keeps running.
///
/// Payload: `{role, slug, turn_id, elapsed_sec, ts}`.
pub const CHAT_TURN_RUNNING_LONG: &str = "chat_turn_running_long";

/// V0.6.8 F195 — build a `chat_turn_running_long` event JSON.
pub fn build_chat_turn_running_long_event(
    role: &str,
    slug: &str,
    turn_id: &str,
    elapsed_sec: u64,
) -> Value {
    serde_json::json!({
        "event": CHAT_TURN_RUNNING_LONG,
        "role": role,
        "slug": slug,
        "turn_id": turn_id,
        "elapsed_sec": elapsed_sec,
        "ts": Utc::now().to_rfc3339(),
    })
}

/// `chat_turn_timeout` — V0.6.8 F195. Per-turn watchdog crossed the
/// second threshold (`2× turn_timeout_sec`, default 180s) without ever
/// seeing `chat_turn_completed`. Carries a `stuck: true` flag so
/// downstream consumers (web UI, ops dashboards) can distinguish from
/// a slow-but-progressing turn. Hard rule: **does not kill the turn**
/// — the watchdog only surfaces the silent stall; recovery is via
/// user-driven `/clear` / signal-reset, not engine intervention (R5
/// 守 from CLAUDE.md §三).
///
/// Payload: `{role, slug, turn_id, elapsed_sec, stuck, ts}`.
pub const CHAT_TURN_TIMEOUT: &str = "chat_turn_timeout";

/// V0.6.8 F195 — build a `chat_turn_timeout` event JSON. The `stuck`
/// field is hard-coded `true`; it exists in the payload so a future
/// `chat_turn_timeout_recovered` event (if we ever add one when the
/// turn does eventually finish post-timeout) can be distinguished on
/// the wire by flipping the flag.
pub fn build_chat_turn_timeout_event(
    role: &str,
    slug: &str,
    turn_id: &str,
    elapsed_sec: u64,
) -> Value {
    serde_json::json!({
        "event": CHAT_TURN_TIMEOUT,
        "role": role,
        "slug": slug,
        "turn_id": turn_id,
        "elapsed_sec": elapsed_sec,
        "stuck": true,
        "ts": Utc::now().to_rfc3339(),
    })
}

// ---------------- V0.6.1 F98 plan-approval event kinds ----------------

/// `plan_pending` — agent wrote a plan markdown to
/// `<project>/.ccteam/plans/<agent>-*.md` and the orchestrator has
/// noticed it. Payload:
/// `{plan_id, agent, plan_path, outbox, timeout_min, ts}`.
pub const PLAN_PENDING: &str = "plan_pending";

/// `plan_decision` — user replied `APPROVE` / `REJECT` / `EDIT
/// <comment>` via the configured IM outbox; the engine has translated
/// it to a decision file the agent reads on resume. Payload:
/// `{plan_id, agent, decision, comment?, ts}`.
pub const PLAN_DECISION: &str = "plan_decision";

/// `plan_timeout` — `timeout_min` elapsed without a user reply.
/// Payload: `{plan_id, agent, on_timeout, ts}`. The engine may emit a
/// follow-up `plan_decision` synthesized from `on_timeout: auto-approve
/// | reject`, or leave the plan paused when `on_timeout: escalate`.
pub const PLAN_TIMEOUT: &str = "plan_timeout";

/// True if `kind` is one of the F98 plan-approval event names.
pub fn is_plan_event(kind: &str) -> bool {
    matches!(kind, PLAN_PENDING | PLAN_DECISION | PLAN_TIMEOUT)
}

/// Build a `plan_pending` event JSON.
pub fn build_plan_pending_event(
    plan_id: &str,
    agent: &str,
    plan_path: &str,
    outbox: &str,
    timeout_min: u32,
) -> Value {
    serde_json::json!({
        "event": PLAN_PENDING,
        "plan_id": plan_id,
        "agent": agent,
        "plan_path": plan_path,
        "outbox": outbox,
        "timeout_min": timeout_min,
        "ts": Utc::now().to_rfc3339(),
    })
}

/// Build a `plan_decision` event JSON. `comment` is the optional
/// free-text trailer parsed from `EDIT <comment>` or `REJECT <reason>`.
pub fn build_plan_decision_event(
    plan_id: &str,
    agent: &str,
    decision: &str,
    comment: Option<&str>,
) -> Value {
    let mut v = serde_json::json!({
        "event": PLAN_DECISION,
        "plan_id": plan_id,
        "agent": agent,
        "decision": decision,
        "ts": Utc::now().to_rfc3339(),
    });
    if let Some(c) = comment {
        v.as_object_mut()
            .unwrap()
            .insert("comment".to_string(), Value::String(c.to_string()));
    }
    v
}

/// Build a `plan_timeout` event JSON.
pub fn build_plan_timeout_event(plan_id: &str, agent: &str, on_timeout: &str) -> Value {
    serde_json::json!({
        "event": PLAN_TIMEOUT,
        "plan_id": plan_id,
        "agent": agent,
        "on_timeout": on_timeout,
        "ts": Utc::now().to_rfc3339(),
    })
}

// ---------------- V0.8 rmux W4-fu Codex app-server notifications ----------------
//
// These four events surface Codex-only mode-3 notifications that the
// W4 `initialize` handshake (`experimentalApi: true`) unlocked. They are
// **additive observability rows**, deliberately distinct from the F98
// plan-approval events above:
//
// - `codex_plan_updated` is Codex's `update_plan` todo/checklist tool
//   output (the upstream source itself notes "`update_plan` is a
//   todo/checklist tool; it is not related to plan-mode updates" —
//   `references/codex/codex-rs/app-server/src/bespoke_event_handling.rs`).
//   It is a fire-and-forget streaming progress signal (the analog of
//   Claude's `TodoWrite` hook output), NOT a HITL pause point — Codex
//   never awaits a client response after emitting it. The real Codex
//   HITL approval path is `thread/status/changed → Active{WaitingOnApproval}`
//   plus server-initiated `item/*/requestApproval` requests, which is a
//   separate (future) `plan_pending` wiring. Mapping `turn/plan/updated`
//   onto `plan_pending` would spuriously fire the F98 IM round-trip on
//   every checklist tick, so we keep it a pure observability event.

/// `codex_plan_updated` — Codex emitted a `turn/plan/updated`
/// notification (its `update_plan` todo/checklist tool). Payload:
/// `{thread_id, turn_id, explanation?, plan: [{step, status}], vendor:"codex", ts}`.
/// `status` is one of `pending` / `inProgress` / `completed` (camelCase
/// wire enum). Observability only — NOT a HITL approval pause.
pub const CODEX_PLAN_UPDATED: &str = "codex_plan_updated";

/// `codex_token_usage` — Codex emitted a `thread/tokenUsage/updated`
/// notification (mid-turn token accounting). Payload:
/// `{thread_id, turn_id, total:{...}, last:{...}, model_context_window?, vendor:"codex", ts}`.
/// This is **not** a cost-ledger write — the authoritative cost row is
/// still the `agent_done` written at `turn/completed`. This event exists
/// so a budget tripwire can fire mid-turn before a runaway turn completes.
pub const CODEX_TOKEN_USAGE: &str = "codex_token_usage";

/// `codex_thread_status` — Codex emitted a `thread/status/changed`
/// notification. Payload:
/// `{thread_id, status, active_flags:[...], vendor:"codex", ts}`.
/// `status` is `not_loaded` / `idle` / `system_error` / `active` (the
/// internally-tagged `type` discriminator, snake_cased here);
/// `active_flags` carries `waiting_on_approval` / `waiting_on_user_input`
/// when `status == active`. The `waiting_on_approval` flag is the
/// authoritative "this Codex thread is blocked on a human" signal that a
/// future `mode: human-approval` Codex adapter will combine with the
/// server-initiated `item/*/requestApproval` handlers.
pub const CODEX_THREAD_STATUS: &str = "codex_thread_status";

/// `codex_rate_limit` — Codex emitted an `account/rateLimits/updated`
/// notification (typed rate-limit visibility, replacing TUI scrape).
/// Payload: `{primary?:{used_percent, window_duration_mins?, resets_at?},
/// secondary?:{...}, rate_limit_reached_type?, plan_type?, vendor:"codex", ts}`.
/// Feeds the F84 budget-cap escalation surface with typed numbers instead
/// of a string-matched TUI error.
pub const CODEX_RATE_LIMIT: &str = "codex_rate_limit";

/// True if `kind` is one of the V0.8 Codex app-server notification event
/// names.
pub fn is_codex_notification_event(kind: &str) -> bool {
    matches!(
        kind,
        CODEX_PLAN_UPDATED | CODEX_TOKEN_USAGE | CODEX_THREAD_STATUS | CODEX_RATE_LIMIT
    )
}

/// Build a `codex_plan_updated` event JSON. `plan` is the verbatim
/// `Vec<TurnPlanStep>` array from the wire (`[{step, status}, ...]`).
pub fn build_codex_plan_updated_event(
    thread_id: &str,
    turn_id: &str,
    explanation: Option<&str>,
    plan: Value,
) -> Value {
    let mut v = serde_json::json!({
        "event": CODEX_PLAN_UPDATED,
        "vendor": "codex",
        "thread_id": thread_id,
        "turn_id": turn_id,
        "plan": plan,
        "ts": Utc::now().to_rfc3339(),
    });
    if let Some(e) = explanation {
        v.as_object_mut()
            .unwrap()
            .insert("explanation".to_string(), Value::String(e.to_string()));
    }
    v
}

/// Build a `codex_token_usage` event JSON. `total` / `last` are the
/// verbatim `TokenUsageBreakdown` objects from the wire.
pub fn build_codex_token_usage_event(
    thread_id: &str,
    turn_id: &str,
    total: Value,
    last: Value,
    model_context_window: Option<i64>,
) -> Value {
    let mut v = serde_json::json!({
        "event": CODEX_TOKEN_USAGE,
        "vendor": "codex",
        "thread_id": thread_id,
        "turn_id": turn_id,
        "total": total,
        "last": last,
        "ts": Utc::now().to_rfc3339(),
    });
    if let Some(w) = model_context_window {
        v.as_object_mut()
            .unwrap()
            .insert("model_context_window".to_string(), Value::Number(w.into()));
    }
    v
}

/// Build a `codex_thread_status` event JSON. `status` is the snake_cased
/// thread-status discriminator; `active_flags` is the (possibly empty)
/// list of snake_cased active flags.
pub fn build_codex_thread_status_event(
    thread_id: &str,
    status: &str,
    active_flags: Vec<String>,
) -> Value {
    serde_json::json!({
        "event": CODEX_THREAD_STATUS,
        "vendor": "codex",
        "thread_id": thread_id,
        "status": status,
        "active_flags": active_flags,
        "ts": Utc::now().to_rfc3339(),
    })
}

/// Build a `codex_rate_limit` event JSON. `snapshot` is the verbatim
/// `RateLimitSnapshot` object from the wire (camelCase keys preserved).
pub fn build_codex_rate_limit_event(snapshot: Value) -> Value {
    serde_json::json!({
        "event": CODEX_RATE_LIMIT,
        "vendor": "codex",
        "snapshot": snapshot,
        "ts": Utc::now().to_rfc3339(),
    })
}

/// Build a `typed_event` observability row from the rmux typed-event
/// pipeline. These surface daemon-side pattern detections (rate-limit /
/// context-overflow / idle / process-exit) merged through the
/// EnrichedEvent merger. They exist for **visibility only** — NOTHING
/// currently acts on them. `event_kind` is a stable snake_case kind
/// string, `captured` the lossy P2/P3 detail, `session` the mux session
/// identity.
pub fn build_typed_event_event(
    vendor: &str,
    event_kind: &str,
    captured: &str,
    session: &str,
) -> Value {
    serde_json::json!({
        "kind": "typed_event",
        "vendor": vendor,
        "event_kind": event_kind,
        "captured": captured,
        "session": session,
        "ts": Utc::now().to_rfc3339(),
    })
}

/// Build a `merger_lossy_partial` row from the rmux typed-event pipeline
/// (V0.8 Slice 2). Emitted when a lossy P2 pattern fired (e.g. a `turn_done`
/// pane match) but the lossless P1 enrichment (the Claude `Stop` hook) never
/// arrived within the merger's grace window — i.e. a turn whose authoritative
/// hook was lost (crashed hook subprocess, etc.). For **visibility only** —
/// nothing acts on these. Same field layout as [`build_typed_event_event`]
/// (parse either with one struct, branch on `kind`).
pub fn build_merger_lossy_partial_event(
    vendor: &str,
    event_kind: &str,
    captured: &str,
    session: &str,
) -> Value {
    serde_json::json!({
        "kind": "merger_lossy_partial",
        "vendor": vendor,
        "event_kind": event_kind,
        "captured": captured,
        "session": session,
        "ts": Utc::now().to_rfc3339(),
    })
}

/// True if `kind` is one of the chat-mode event names (F108 / F118 /
/// F192c / F195 / F196).
pub fn is_chat_event(kind: &str) -> bool {
    matches!(
        kind,
        CHAT_SESSION_STARTED
            | CHAT_TURN_USER_PROMPT
            | CHAT_TURN_COMPLETED
            | CHAT_SESSION_RESET
            | CHAT_SESSION_RESET_WITH_RECOVERY
            | CHAT_COMPACT_DONE
            | CHAT_HOP_ESCALATE
            | CHAT_BOT_PERMANENT_FAILURE
            | CHAT_MARKER_SELF_HEAL_ATTEMPT
            | CHAT_BOT_MARKER_STUCK
            | CHAT_TURN_RUNNING_LONG
            | CHAT_TURN_TIMEOUT
    )
}

// ---------------- V0.4.0 F67 workflow event aggregations ----------------

/// Status of one agent session inferred from the `agent_spawn` /
/// `agent_done` event pair.
///
/// `Running` — `agent_spawn` was seen without a matching `agent_done`
/// for the same `(role, session_id)`.
/// `Done { cost_usd }` — terminal `agent_done` with `status` in
/// `{"completed", "stopped"}`. `cost_usd` defaults to `0.0` when the
/// event omits the field.
/// `Errored` — terminal `agent_done` with any other `status` (e.g.
/// `"error"`); F66 still writes `cost_usd` but the dispatch failed.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AgentSessionStatus {
    /// Session has not yet emitted `agent_done`.
    Running,
    /// Session terminated normally. `cost_usd` mirrors F66's
    /// `agent_done.cost_usd` field (0.0 when the harness reported no
    /// cost).
    Done { cost_usd: f64 },
    /// Session terminated with a non-success `status`.
    Errored { cost_usd: f64 },
}

/// One agent session summary derived from progress.jsonl events.
/// `started_at` is the `agent_spawn` event's `ts` (parsed RFC3339; if
/// the field is missing or unparseable the helper uses the current
/// wall-clock time at parse, which is harmless for an event that
/// already preceded `now`).
#[derive(Debug, Clone, Serialize)]
pub struct AgentSessionSummary {
    pub role: String,
    pub session_id: String,
    pub started_at: DateTime<Utc>,
    pub status: AgentSessionStatus,
}

/// Sum `cost_usd` across every `agent_done` event in the slice.
///
/// F66 writes the per-session cost on the terminal `agent_done` event
/// (NOT on `agent_spawn`; the harness only knows the cost once the
/// session ends). Missing or non-numeric `cost_usd` fields contribute
/// `0.0`.
pub fn workflow_cost_total(events: &[Value]) -> f64 {
    events
        .iter()
        .filter(|e| e.get("event").and_then(|s| s.as_str()) == Some("agent_done"))
        .map(|e| e.get("cost_usd").and_then(|v| v.as_f64()).unwrap_or(0.0))
        .sum()
}

/// Count `escalation` events in the slice. Escalations come from two
/// F66 codepaths: the 3-strike `spawn_failed` fix-loop and the
/// budget-exceeded guard's `send_btw_escalation` (which writes an
/// `escalation` event before the inbox push). Both surface here as a
/// single integer for `WorkflowSummary.escalation_count`.
pub fn escalation_count(events: &[Value]) -> u32 {
    events
        .iter()
        .filter(|e| e.get("event").and_then(|s| s.as_str()) == Some("escalation"))
        .count() as u32
}

/// Walk events and reconstruct each agent session's status from the
/// `agent_spawn` / `agent_done` pair (matched by `session_id`).
///
/// Output order is deterministic: sessions are sorted by `started_at`
/// ascending, then by `session_id` as a tiebreaker. This keeps tests
/// and UI rows stable across runs.
///
/// Sessions whose `agent_spawn` lacks a `session_id` field are
/// skipped (they cannot be paired with a later `agent_done`).
///
/// **Pure function.** Always returns `AgentSessionStatus::Running`
/// for any spawn without a matching `agent_done`, regardless of
/// whether the underlying claude bg job is still alive. The web /
/// orchestrator caller layers V0.4.5 F80 liveness probing on top
/// via [`current_agent_sessions_with_liveness`] — keeping this fn
/// pure preserves the existing test suite + lets schema-level unit
/// tests stay IO-free.
pub fn current_agent_sessions(events: &[Value]) -> Vec<AgentSessionSummary> {
    current_agent_sessions_inner(events, None::<&dyn Fn(Option<&str>) -> _>)
}

/// V0.4.5 F80 — liveness-aware sibling of [`current_agent_sessions`].
///
/// Same accounting as the pure version, but after the spawn/done
/// pairing pass every `Running` entry is cross-referenced against
/// the caller's `liveness` closure. The closure receives the
/// `job_id` recorded on the originating `agent_spawn` event
/// (`None` for legacy / pre-F80 rows) and returns the liveness
/// verdict.
///
/// Terminal verdicts demote `Running` → `Done` / `Errored` with the
/// closure's reported `cost_usd`, matching the shape the SPA already
/// renders for genuinely-finished sessions. The pure
/// `current_agent_sessions` API stays untouched so existing callers
/// + unit tests are unaffected.
///
/// **Side-effect-free.** This function does not write to
/// `progress.jsonl`; the matching cleanup `agent_done` is emitted
/// by `orchestrator::poll_completions` (the only consumer authorised
/// to write workflow events). The function just makes the read-side
/// UI consistent immediately, before the orchestrator's next tick.
pub fn current_agent_sessions_with_liveness<F>(
    events: &[Value],
    liveness: F,
) -> Vec<AgentSessionSummary>
where
    F: Fn(Option<&str>) -> crate::claude_job::JobLiveness,
{
    current_agent_sessions_inner(events, Some(&liveness))
}

/// Closure type alias for the optional liveness probe injected into
/// `current_agent_sessions_inner`. Carries an explicit lifetime so the
/// caller's closure does not need to be `'static` (the public
/// `current_agent_sessions_with_liveness` helper takes a generic `F: Fn`
/// and reborrows it as a short-lived trait object).
pub type LivenessProbe<'a> = dyn Fn(Option<&str>) -> crate::claude_job::JobLiveness + 'a;

fn current_agent_sessions_inner<'a>(
    events: &[Value],
    liveness: Option<&LivenessProbe<'a>>,
) -> Vec<AgentSessionSummary> {
    // `BTreeMap` keyed by session_id keeps a single entry per session
    // (the last terminal `agent_done` wins if for some reason two
    // arrive).
    let mut by_sid: BTreeMap<String, AgentSessionSummary> = BTreeMap::new();
    // V0.4.5 F80 — remember each session's `agent_spawn::job_id`
    // (if any) so the optional liveness probe can run after the
    // first pass.
    let mut job_ids: BTreeMap<String, Option<String>> = BTreeMap::new();

    for event in events {
        let kind = event.get("event").and_then(|s| s.as_str()).unwrap_or("");
        let sid = match event.get("session_id").and_then(|s| s.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => continue,
        };
        let role = event
            .get("role")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();

        match kind {
            "agent_spawn" => {
                let started_at = parse_ts(event.get("ts").and_then(|s| s.as_str()));
                let job_id = event
                    .get("job_id")
                    .and_then(|s| s.as_str())
                    .map(String::from);
                by_sid.entry(sid.clone()).or_insert(AgentSessionSummary {
                    role,
                    session_id: sid.clone(),
                    started_at,
                    status: AgentSessionStatus::Running,
                });
                job_ids.entry(sid).or_insert(job_id);
            }
            "agent_done" => {
                let cost_usd = event
                    .get("cost_usd")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let status_str = event
                    .get("status")
                    .and_then(|s| s.as_str())
                    .unwrap_or("completed");
                let status = match status_str {
                    "completed" | "stopped" => AgentSessionStatus::Done { cost_usd },
                    _ => AgentSessionStatus::Errored { cost_usd },
                };
                // Update existing entry if `agent_spawn` was already
                // observed; otherwise synthesise from this event only
                // (rare: progress.jsonl truncation, but defensible).
                by_sid
                    .entry(sid.clone())
                    .and_modify(|entry| entry.status = status.clone())
                    .or_insert(AgentSessionSummary {
                        role: role.clone(),
                        session_id: sid,
                        started_at: parse_ts(event.get("ts").and_then(|s| s.as_str())),
                        status,
                    });
            }
            _ => {}
        }
    }

    // V0.4.5 F80 — second pass: demote phantom `Running` entries
    // whose claude bg job is gone (state.json missing, firstTerminalAt
    // non-null, or state is terminal).
    if let Some(probe) = liveness {
        for (sid, entry) in by_sid.iter_mut() {
            if !matches!(entry.status, AgentSessionStatus::Running) {
                continue;
            }
            let job_id = job_ids.get(sid).and_then(|opt| opt.as_deref());
            match probe(job_id) {
                crate::claude_job::JobLiveness::Running => {}
                crate::claude_job::JobLiveness::Terminal { status, cost_usd } => {
                    entry.status = match status {
                        "completed" | "stopped" => AgentSessionStatus::Done { cost_usd },
                        _ => AgentSessionStatus::Errored { cost_usd },
                    };
                }
            }
        }
    }

    let mut out: Vec<AgentSessionSummary> = by_sid.into_values().collect();
    out.sort_by(|a, b| {
        a.started_at
            .cmp(&b.started_at)
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
    out
}

/// V0.4.5 F80 — extract `(session_id, job_id, role)` triples from
/// every `agent_spawn` event in `events` that does **not** yet have
/// a matching `agent_done`. Used by
/// `orchestrator::poll_completions` to drive the stale-spawn cleanup
/// scan (one `agent_done` per phantom row).
///
/// Pure. Caller-controlled IO: typically each `(sid, job_id, role)`
/// is fed into [`crate::claude_job::probe_job`] and, when terminal,
/// translated into a synthetic `agent_done` event the orchestrator
/// appends to `progress.jsonl`.
pub fn open_agent_spawns(events: &[Value]) -> Vec<(String, Option<String>, String)> {
    let mut spawns: BTreeMap<String, (Option<String>, String)> = BTreeMap::new();
    let mut closed: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for event in events {
        let kind = event.get("event").and_then(|s| s.as_str()).unwrap_or("");
        let sid = match event.get("session_id").and_then(|s| s.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => continue,
        };
        match kind {
            "agent_spawn" => {
                let job_id = event
                    .get("job_id")
                    .and_then(|s| s.as_str())
                    .map(String::from);
                let role = event
                    .get("role")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                spawns.entry(sid).or_insert((job_id, role));
            }
            "agent_done" => {
                closed.insert(sid);
            }
            _ => {}
        }
    }
    spawns
        .into_iter()
        .filter(|(sid, _)| !closed.contains(sid))
        .map(|(sid, (job_id, role))| (sid, job_id, role))
        .collect()
}

/// V0.8 W3 follow-up — one open (un-`agent_done`-ed) `agent_spawn` row
/// with the extra mode-2-via-mux markers the orchestrator needs to pick
/// the right liveness probe. A sibling of [`open_agent_spawns`] (kept
/// stable for `queries.rs`) so callers that don't care about mux keep
/// the simpler triple.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenAgentSpawn {
    pub session_id: String,
    pub job_id: Option<String>,
    pub role: String,
    /// `true` when the `agent_spawn` row was written by a
    /// `CCTEAM_CLAUDE_BG_VIA_MUX=1` foreground-in-mux spawn — its
    /// liveness lives in the mux session lifecycle, NOT in
    /// `~/.claude/jobs/<id>/state.json`.
    pub via_mux: bool,
    /// Mux session name to probe via `MuxBackend::exists` when
    /// `via_mux` is set. `None` for legacy `--bg` + codex rows.
    pub mux_session: Option<String>,
}

/// V0.8 W3 follow-up — like [`open_agent_spawns`] but also surfaces the
/// `via_mux` / `mux_session` markers persisted on the `agent_spawn`
/// event so the orchestrator's stale-spawn pass can route mode-2
/// foreground-in-mux spawns through the mux session lifecycle instead
/// of the F80 `state.json` probe (which never exists for them).
pub fn open_agent_spawns_detailed(events: &[Value]) -> Vec<OpenAgentSpawn> {
    let mut spawns: BTreeMap<String, OpenAgentSpawn> = BTreeMap::new();
    let mut closed: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for event in events {
        let kind = event.get("event").and_then(|s| s.as_str()).unwrap_or("");
        let sid = match event.get("session_id").and_then(|s| s.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => continue,
        };
        match kind {
            "agent_spawn" => {
                let job_id = event
                    .get("job_id")
                    .and_then(|s| s.as_str())
                    .map(String::from);
                let role = event
                    .get("role")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                let via_mux = event
                    .get("via_mux")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let mux_session = event
                    .get("mux_session")
                    .and_then(|s| s.as_str())
                    .map(String::from);
                spawns.entry(sid.clone()).or_insert(OpenAgentSpawn {
                    session_id: sid,
                    job_id,
                    role,
                    via_mux,
                    mux_session,
                });
            }
            "agent_done" => {
                closed.insert(sid);
            }
            _ => {}
        }
    }
    spawns
        .into_values()
        .filter(|s| !closed.contains(&s.session_id))
        .collect()
}

fn parse_ts(raw: Option<&str>) -> DateTime<Utc> {
    raw.and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(Utc::now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn idle_when_no_events_yet() {
        assert!(is_idle(None));
    }

    #[test]
    fn idle_after_stop() {
        let e = json!({"event": "Stop", "ts": "..."});
        assert!(is_idle(Some(&e)));
    }

    #[test]
    fn idle_after_notification() {
        let e = json!({"event": "notification"});
        assert!(is_idle(Some(&e)));
    }

    #[test]
    fn busy_during_tool_use() {
        let e = json!({"event": "PreToolUse", "tool": "Edit"});
        assert!(!is_idle(Some(&e)));
        let e = json!({"event": "PostToolUse"});
        assert!(!is_idle(Some(&e)));
        let e = json!({"event": "phase_inject"});
        assert!(!is_idle(Some(&e)));
    }

    #[test]
    fn phase_boundaries_are_idle() {
        for kind in ["session_start", "phase_done", "escalate", "SessionEnd"] {
            let e = json!({"event": kind});
            assert!(is_idle(Some(&e)), "{kind} should be treated as idle");
        }
    }

    #[test]
    fn idle_treats_subagent_stop_as_idle() {
        // E2E 2026-05-06 F1+F2: Claude Code emits SubagentStop 2–5 s
        // after Stop whenever a turn used Task. The main loop is already
        // idle at that point — classifying it as busy caused the next
        // phase inject to be wrapped in `/btw`, which spawns a toolless
        // side-agent that cannot execute the next phase.
        let e = json!({"event": "SubagentStop"});
        assert!(is_idle(Some(&e)));
    }

    #[test]
    fn idle_aware_message_wraps_with_btw_when_busy() {
        assert_eq!(idle_aware_message("hello", true), "hello");
        assert_eq!(idle_aware_message("hello", false), "/btw hello");
    }

    // ---------------- V0.2.2 F36 subagent_active helper ----------------

    fn pretool_task() -> Value {
        json!({"event": "PreToolUse", "tool": "Task"})
    }
    fn pretool_other(tool: &str) -> Value {
        json!({"event": "PreToolUse", "tool": tool})
    }
    fn subagent_stop() -> Value {
        json!({"event": "SubagentStop"})
    }

    #[test]
    fn subagent_active_empty_log_returns_false() {
        assert!(!subagent_active(&[]));
    }

    #[test]
    fn subagent_active_open_window_after_pretool_task() {
        let events = [
            json!({"event": "phase_inject", "phase": "implement"}),
            pretool_task(),
        ];
        assert!(subagent_active(&events));
    }

    #[test]
    fn subagent_active_paired_pretool_task_and_subagent_stop_returns_false() {
        let events = [pretool_task(), subagent_stop()];
        assert!(!subagent_active(&events));
    }

    #[test]
    fn subagent_active_nested_task_calls_open_two_windows() {
        // outer Task launched, inner Task launched, only one SubagentStop
        // arrived so far → still one open window.
        let events = [pretool_task(), pretool_task(), subagent_stop()];
        assert!(subagent_active(&events));
    }

    #[test]
    fn subagent_active_old_subagent_stop_does_not_close_new_pretool_task() {
        // Old paired sequence (closed) followed by a fresh PreToolUse(Task)
        // with no follow-up — the new window must register as active.
        let events = [
            pretool_task(),
            subagent_stop(),
            json!({"event": "PostToolUse", "tool": "Read"}),
            pretool_task(),
        ];
        assert!(subagent_active(&events));
    }

    #[test]
    fn subagent_active_ignores_non_task_pretool() {
        let events = [pretool_other("Read"), pretool_other("Edit")];
        assert!(!subagent_active(&events));
    }

    // ---------------- V0.6.0 F108 chat-mode event builders ----------------

    #[test]
    fn chat_event_constants_match_expected_strings() {
        assert_eq!(CHAT_SESSION_STARTED, "chat_session_started");
        assert_eq!(CHAT_TURN_USER_PROMPT, "chat_turn_user_prompt");
        assert_eq!(CHAT_TURN_COMPLETED, "chat_turn_completed");
        assert_eq!(CHAT_SESSION_RESET, "chat_session_reset");
        assert_eq!(
            CHAT_SESSION_RESET_WITH_RECOVERY,
            "chat_session_reset_with_recovery"
        );
        assert_eq!(CHAT_COMPACT_DONE, "chat_compact_done");
        assert_eq!(CHAT_HOP_ESCALATE, "chat_hop_escalate");
    }

    #[test]
    fn is_chat_event_recognises_all_chat_kinds() {
        for kind in [
            CHAT_SESSION_STARTED,
            CHAT_TURN_USER_PROMPT,
            CHAT_TURN_COMPLETED,
            CHAT_SESSION_RESET,
            CHAT_SESSION_RESET_WITH_RECOVERY,
            CHAT_COMPACT_DONE,
            CHAT_HOP_ESCALATE,
            CHAT_BOT_PERMANENT_FAILURE,
            CHAT_TURN_RUNNING_LONG,
            CHAT_TURN_TIMEOUT,
        ] {
            assert!(is_chat_event(kind), "{kind} should be a chat event");
        }
        assert!(!is_chat_event("Stop"));
        assert!(!is_chat_event("agent_done"));
    }

    #[test]
    fn build_chat_turn_running_long_event_shape() {
        let ev = build_chat_turn_running_long_event("alice", "dev-foo", "turn-42", 95);
        assert_eq!(ev["event"], CHAT_TURN_RUNNING_LONG);
        assert_eq!(ev["role"], "alice");
        assert_eq!(ev["slug"], "dev-foo");
        assert_eq!(ev["turn_id"], "turn-42");
        assert_eq!(ev["elapsed_sec"], 95);
        assert!(ev["ts"].is_string());
    }

    #[test]
    fn build_chat_turn_timeout_event_carries_stuck_flag() {
        let ev = build_chat_turn_timeout_event("alice", "dev-foo", "turn-42", 200);
        assert_eq!(ev["event"], CHAT_TURN_TIMEOUT);
        assert_eq!(ev["role"], "alice");
        assert_eq!(ev["slug"], "dev-foo");
        assert_eq!(ev["turn_id"], "turn-42");
        assert_eq!(ev["elapsed_sec"], 200);
        assert_eq!(ev["stuck"], true);
    }

    #[test]
    fn build_chat_session_started_event_shape() {
        let ev = build_chat_session_started_event("alice", "/home/u/projects/dev-foo");
        assert_eq!(ev["event"], CHAT_SESSION_STARTED);
        assert_eq!(ev["role"], "alice");
        assert_eq!(ev["project_dir"], "/home/u/projects/dev-foo");
        assert!(ev["ts"].is_string());
    }

    #[test]
    fn build_chat_turn_user_prompt_event_truncates_long_excerpt() {
        let long = "x".repeat(1000);
        let ev = build_chat_turn_user_prompt_event("bob", "turn-42", &long);
        assert_eq!(ev["event"], CHAT_TURN_USER_PROMPT);
        let excerpt = ev["prompt_excerpt"].as_str().unwrap();
        assert_eq!(excerpt.chars().count(), 256);
    }

    #[test]
    fn build_chat_turn_completed_event_carries_usage() {
        let usage = crate::harness::UnifiedTokenUsage::default();
        let ev = build_chat_turn_completed_event("carol", "turn-7", &usage);
        assert_eq!(ev["event"], CHAT_TURN_COMPLETED);
        assert_eq!(ev["turn_id"], "turn-7");
        assert!(ev["usage"].is_object());
    }

    #[test]
    fn build_chat_hop_escalate_event_shape() {
        let ev = build_chat_hop_escalate_event("dora", 3, "eve");
        assert_eq!(ev["event"], CHAT_HOP_ESCALATE);
        assert_eq!(ev["hop_count"], 3);
        assert_eq!(ev["last_bot"], "eve");
    }

    #[test]
    fn is_idle_treats_chat_terminal_boundaries_as_idle() {
        for kind in [
            CHAT_TURN_COMPLETED,
            CHAT_SESSION_STARTED,
            CHAT_SESSION_RESET,
            CHAT_SESSION_RESET_WITH_RECOVERY,
            CHAT_COMPACT_DONE,
        ] {
            let e = json!({"event": kind});
            assert!(is_idle(Some(&e)), "{kind} should be treated as idle");
        }
    }

    #[test]
    fn is_idle_treats_chat_user_prompt_as_busy() {
        // User just submitted a turn → claude is processing → busy.
        let e = json!({"event": CHAT_TURN_USER_PROMPT});
        assert!(!is_idle(Some(&e)));
    }

    #[test]
    fn build_chat_session_reset_with_recovery_event_carries_count() {
        let ev = build_chat_session_reset_with_recovery_event("frank", 12);
        assert_eq!(ev["event"], CHAT_SESSION_RESET_WITH_RECOVERY);
        assert_eq!(ev["recovered_turns"], 12);
    }

    #[test]
    fn build_chat_marker_self_heal_attempt_event_shape() {
        // V0.6.8 F196 — attempt_n 1-based, carries role + ts, no
        // surprise fields. Web SSE / api_v1 consumers handle this
        // untyped (per F192c verification — same envelope).
        let ev = build_chat_marker_self_heal_attempt_event("grace", 2);
        assert_eq!(ev["event"], CHAT_MARKER_SELF_HEAL_ATTEMPT);
        assert_eq!(ev["role"], "grace");
        assert_eq!(ev["attempt_n"], 2);
        assert!(ev["ts"].is_string());
    }

    #[test]
    fn build_chat_bot_marker_stuck_event_shape() {
        // V0.6.8 F196 — same envelope as F192c
        // chat_bot_permanent_failure: role + attempts + ts. No
        // freeform reason field because the failure mode is
        // structural (SessionStart hook prerequisite missing) and
        // the operator-facing surface for diagnostics is the F187
        // tail_marker_missing WARN line + the supervisor's heal
        // attempt history.
        let ev = build_chat_bot_marker_stuck_event("hank", 3);
        assert_eq!(ev["event"], CHAT_BOT_MARKER_STUCK);
        assert_eq!(ev["role"], "hank");
        assert_eq!(ev["attempts"], 3);
        assert!(ev["ts"].is_string());
    }

    #[test]
    fn subagent_active_extra_subagent_stops_do_not_underflow() {
        // Defensive: stray SubagentStop events with no matching open
        // window must not panic / wrap around.
        let events = [subagent_stop(), subagent_stop(), pretool_task()];
        assert!(subagent_active(&events));
    }
}
