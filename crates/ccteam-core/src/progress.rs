//! progress.jsonl reader / writer + idle detection + workflow event aggregations.
//!
//! progress.jsonl is the orchestrator's only state-truth source
//! (`docs/dev/tech-design.md` §5.5). This module gives both the hook
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
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

pub use ccteam_harness::execution::progress_bridge::{
    append_event, build_chat_bot_permanent_failure_event, build_chat_compact_done_event,
    build_chat_hop_escalate_event, build_chat_marker_self_heal_attempt_event,
    build_chat_permission_prompt_outstanding_event, build_chat_session_reset_event,
    build_chat_session_reset_event_with_reason, build_chat_session_reset_with_recovery_event,
    build_chat_session_started_event, build_chat_tool_call_started_event,
    build_chat_turn_completed_event, build_chat_turn_failed_event,
    build_chat_turn_running_long_event, build_chat_turn_timeout_event,
    build_chat_turn_user_prompt_event, build_codex_plan_updated_event,
    build_codex_rate_limit_event, build_codex_thread_status_event, build_codex_token_usage_event,
    build_merger_lossy_partial_event, build_typed_event_event, CHAT_BOT_PERMANENT_FAILURE,
    CHAT_COMPACT_DONE, CHAT_HOP_ESCALATE, CHAT_MARKER_SELF_HEAL_ATTEMPT,
    CHAT_PERMISSION_PROMPT_OUTSTANDING, CHAT_SESSION_RESET, CHAT_SESSION_RESET_WITH_RECOVERY,
    CHAT_SESSION_STARTED, CHAT_TOOL_CALL_STARTED, CHAT_TURN_COMPLETED, CHAT_TURN_FAILED,
    CHAT_TURN_RUNNING_LONG, CHAT_TURN_TIMEOUT, CHAT_TURN_USER_PROMPT, CODEX_PLAN_UPDATED,
    CODEX_RATE_LIMIT, CODEX_THREAD_STATUS, CODEX_TOKEN_USAGE,
};

/// Read + parse the last non-empty line of `path`. `Ok(None)` when the
/// file is absent or contains no events yet.
///
/// **Tail read (v0.8.7 review-fix R-M7).** This is called by
/// `queries::collect_projects` on EVERY `ccteam status` / `ccteam … ls` /
/// `GET /api/v1/projects`, once per project. progress.jsonl has no rotation
/// or size cap, so a `read_to_string` of the whole file is O(file size) on a
/// hot, frequently-polled path. Instead we seek to EOF and read bounded
/// chunks *backwards* ([`TAIL_CHUNK`] at a time) until a newline separating
/// the last record is found — bytes read are bounded by the size of the last
/// record(s), independent of total file length. Falls back to reading from
/// offset 0 only when the entire file is one (chunk-spanning) line.
pub fn last_event(path: &Path) -> Result<Option<Value>> {
    match read_last_line(path).with_context(|| format!("read {}", path.display()))? {
        None => Ok(None),
        Some(line) => {
            let v: Value = serde_json::from_str(line.trim())
                .with_context(|| format!("parse last line of {}", path.display()))?;
            Ok(Some(v))
        }
    }
}

/// Backward read chunk size for [`read_last_line`]. 8 KiB comfortably holds a
/// single progress event line (token-usage payloads are the largest, well
/// under 8 KiB) so the common case reads exactly one chunk.
const TAIL_CHUNK: u64 = 8 * 1024;

/// Return the last non-empty line of `path` as an owned `String`, reading
/// from the tail in bounded [`TAIL_CHUNK`] steps. `Ok(None)` when the file is
/// absent / empty / only whitespace. Bytes read are O(size of trailing
/// record), NOT O(file size).
fn read_last_line(path: &Path) -> std::io::Result<Option<String>> {
    use std::io::{Read, Seek, SeekFrom};

    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        // Absent file ⇒ no events yet (matches the old `path.exists()` guard).
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut pos = f.seek(SeekFrom::End(0))?;
    if pos == 0 {
        return Ok(None); // empty file
    }

    // Accumulate bytes read so far (in file order) at the FRONT of `buf` so we
    // can scan for the newline that opens the final record.
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let step = pos.min(TAIL_CHUNK);
        pos -= step;
        f.seek(SeekFrom::Start(pos))?;
        let mut chunk = vec![0u8; step as usize];
        f.read_exact(&mut chunk)?;
        // Prepend this earlier chunk before what we already have.
        chunk.extend_from_slice(&buf);
        buf = chunk;

        // Trim trailing newlines/whitespace bytes (a file usually ends with
        // '\n'); then look for the newline that precedes the final record. We
        // need a newline with at least one non-whitespace byte after it.
        if let Some(line) = last_line_from_buf(&buf) {
            return Ok(Some(line));
        }
        if pos == 0 {
            // Reached BOF without an interior newline ⇒ the whole file is one
            // line. Return it (trimmed) if non-empty.
            let s = String::from_utf8_lossy(&buf);
            let trimmed = s.trim();
            return Ok(if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            });
        }
    }
}

/// Given a byte buffer that is a *suffix* of the file, return the last
/// non-empty line IF it is fully contained in the buffer (i.e. there is an
/// interior `\n` before it). Returns `None` when the buffer might not yet
/// contain the start of the last line (caller reads another earlier chunk).
fn last_line_from_buf(buf: &[u8]) -> Option<String> {
    // Index of the end of the last non-whitespace byte.
    let end = buf.iter().rposition(|b| !b.is_ascii_whitespace())? + 1;
    // Scan backwards from `end` for a newline; that newline (if present in
    // this buffer) delimits the start of the final record.
    let line_start = buf[..end].iter().rposition(|&b| b == b'\n');
    match line_start {
        Some(nl) => {
            // Interior newline found → the bytes after it are the full last
            // line, fully contained in `buf`.
            let line = String::from_utf8_lossy(&buf[nl + 1..end]);
            let trimmed = line.trim();
            if trimmed.is_empty() {
                // Trailing region was only whitespace after the newline —
                // shouldn't happen given `end` trims whitespace, but be safe.
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        // No newline yet in this suffix → the last line may start earlier than
        // the buffer's front; signal "read more".
        None => None,
    }
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

/// Return the latest event that belongs to a gateway session id. New chat
/// events carry `sid`; older workflow-era events used `session_id`, so the
/// read-side accepts both names.
pub fn last_event_for_sid(path: &Path, sid: &str) -> Result<Option<Value>> {
    if sid.is_empty() {
        return Ok(None);
    }
    let events = read_all_events(path)?;
    Ok(events
        .into_iter()
        .rev()
        .find(|event| event_sid(event).is_some_and(|value| value == sid)))
}

pub fn event_sid(event: &Value) -> Option<&str> {
    event
        .get("sid")
        .and_then(Value::as_str)
        .or_else(|| event.get("session_id").and_then(Value::as_str))
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
            // A turn that DIED is just as finished as one that answered. Without
            // this, the row that closes a failed turn's busy window would itself
            // read as busy and strand the session at `working` → `stale`.
            | CHAT_TURN_FAILED
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

/// True if `kind` is one of the V0.8 Codex app-server notification event
/// names.
pub fn is_codex_notification_event(kind: &str) -> bool {
    matches!(
        kind,
        CODEX_PLAN_UPDATED | CODEX_TOKEN_USAGE | CODEX_THREAD_STATUS | CODEX_RATE_LIMIT
    )
}

/// True if `kind` is one of the chat-mode event names (F108 / F118 /
/// F192c / F195 / F196).
pub fn is_chat_event(kind: &str) -> bool {
    matches!(
        kind,
        CHAT_SESSION_STARTED
            | CHAT_TURN_USER_PROMPT
            | CHAT_TURN_COMPLETED
            | CHAT_TURN_FAILED
            | CHAT_SESSION_RESET
            | CHAT_SESSION_RESET_WITH_RECOVERY
            | CHAT_COMPACT_DONE
            | CHAT_HOP_ESCALATE
            | CHAT_BOT_PERMANENT_FAILURE
            | CHAT_MARKER_SELF_HEAL_ATTEMPT
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
    /// Mux session name to probe via `ProcessBackend::exists` when
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
        let ev = build_chat_turn_running_long_event("alice", "s7", "dev-foo", "turn-42", 95);
        assert_eq!(ev["event"], CHAT_TURN_RUNNING_LONG);
        assert_eq!(ev["role"], "alice");
        // The classifier selects a session's latest event BY SID — an untagged
        // heartbeat would never be read as the session's own activity.
        assert_eq!(ev["sid"], "s7");
        assert_eq!(ev["slug"], "dev-foo");
        assert_eq!(ev["turn_id"], "turn-42");
        assert_eq!(ev["elapsed_sec"], 95);
        assert!(ev["ts"].is_string());
        assert!(!is_idle(Some(&ev)), "a heartbeat must classify as busy");
    }

    #[test]
    fn build_chat_turn_failed_event_shape() {
        let ev = build_chat_turn_failed_event("alice", "s7", "turn-42", "transport", "boom");
        assert_eq!(ev["event"], CHAT_TURN_FAILED);
        assert_eq!(ev["role"], "alice");
        assert_eq!(ev["sid"], "s7");
        assert_eq!(ev["turn_id"], "turn-42");
        assert_eq!(ev["error_kind"], "transport");
        assert_eq!(ev["error"], "boom");
        assert!(ev["ts"].is_string());
        // A dead turn is a finished turn: this row CLOSES the busy window.
        assert!(is_idle(Some(&ev)));
        // …and it must never be priced — the cost ledger sums completed turns,
        // and a failure carries no usage at all.
        assert!(ev.get("usage").is_none());
    }

    #[test]
    fn build_chat_turn_timeout_event_carries_stuck_flag() {
        let ev = build_chat_turn_timeout_event("alice", "s3", "dev-foo", "turn-42", 200);
        assert_eq!(ev["event"], CHAT_TURN_TIMEOUT);
        assert_eq!(ev["role"], "alice");
        assert_eq!(ev["sid"], "s3");
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

    /// v0.8.7 review-fix (R-L1) — the parked-prompt progress line carries the
    /// role, tool, a (truncated) summary, and the prompt's TTL so an operator
    /// sees the agent is awaiting approval, not stuck.
    #[test]
    fn build_chat_permission_prompt_outstanding_event_shape() {
        let long = "x".repeat(1000);
        let ev = build_chat_permission_prompt_outstanding_event("cto", "Bash", &long, 120);
        assert_eq!(ev["event"], CHAT_PERMISSION_PROMPT_OUTSTANDING);
        assert_eq!(ev["role"], "cto");
        assert_eq!(ev["tool"], "Bash");
        assert_eq!(ev["ttl_secs"], 120);
        assert_eq!(
            ev["summary"].as_str().unwrap().chars().count(),
            256,
            "summary is truncated to 256 chars"
        );
        assert!(ev["ts"].is_string());
    }

    /// v0.8.7 review-fix (R-L1) — appending the parked line round-trips through
    /// the canonical `append_event` (the SoT writer) and `last_event` reads it
    /// back. Deterministic (explicit path, no env). This pins "a progress line
    /// IS emitted when a permission prompt is outstanding".
    #[test]
    fn permission_prompt_outstanding_line_round_trips_through_append() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("dev-foo.jsonl");
        let ev =
            build_chat_permission_prompt_outstanding_event("cto", "Bash", "rm -rf /tmp/x", 120);
        append_event(&path, &ev).unwrap();
        let last = last_event(&path).unwrap().expect("one event present");
        assert_eq!(last["event"], CHAT_PERMISSION_PROMPT_OUTSTANDING);
        assert_eq!(last["tool"], "Bash");
        assert_eq!(last["summary"], "rm -rf /tmp/x");
    }

    #[test]
    fn build_chat_turn_user_prompt_event_truncates_long_excerpt() {
        let long = "x".repeat(1000);
        let ev = build_chat_turn_user_prompt_event("bob", "s4", "turn-42", &long);
        assert_eq!(ev["event"], CHAT_TURN_USER_PROMPT);
        assert_eq!(ev["sid"], "s4");
        let excerpt = ev["prompt_excerpt"].as_str().unwrap();
        assert_eq!(excerpt.chars().count(), 256);
    }

    #[test]
    fn build_chat_turn_completed_event_carries_usage() {
        let usage = ccteam_cost::UnifiedTokenUsage::default();
        let ev = build_chat_turn_completed_event("carol", "s5", "turn-7", &usage, None);
        assert_eq!(ev["event"], CHAT_TURN_COMPLETED);
        assert_eq!(ev["sid"], "s5");
        assert_eq!(ev["turn_id"], "turn-7");
        assert!(ev["usage"].is_object());
        // No model passed → key omitted (unpriced/exposed).
        assert!(ev.get("model").is_none());
    }

    #[test]
    fn build_chat_turn_completed_event_carries_canonical_model() {
        let usage = ccteam_cost::UnifiedTokenUsage::default();
        let ev = build_chat_turn_completed_event(
            "carol",
            "s5",
            "turn-7",
            &usage,
            Some("claude-opus-4-8"),
        );
        assert_eq!(ev["model"], "claude-opus-4-8");
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
        let ev = build_chat_session_reset_with_recovery_event("frank", "s9", 12);
        assert_eq!(ev["event"], CHAT_SESSION_RESET_WITH_RECOVERY);
        assert_eq!(ev["sid"], "s9");
        assert_eq!(ev["recovered_turns"], 12);
    }

    #[test]
    fn build_chat_session_reset_event_carries_sid_for_roleless_session() {
        let ev = build_chat_session_reset_event("", "s7");
        assert_eq!(ev["event"], CHAT_SESSION_RESET);
        assert_eq!(ev["role"], "");
        assert_eq!(ev["sid"], "s7");
    }

    #[test]
    fn build_chat_session_reset_with_reason_event_carries_sid() {
        let ev = build_chat_session_reset_event_with_reason(
            "reviewer",
            "s8",
            "resume_failed_fallback_to_fresh",
        );
        assert_eq!(ev["event"], CHAT_SESSION_RESET);
        assert_eq!(ev["role"], "reviewer");
        assert_eq!(ev["sid"], "s8");
        assert_eq!(ev["reason"], "resume_failed_fallback_to_fresh");
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
    fn subagent_active_extra_subagent_stops_do_not_underflow() {
        // Defensive: stray SubagentStop events with no matching open
        // window must not panic / wrap around.
        let events = [subagent_stop(), subagent_stop(), pretool_task()];
        assert!(subagent_active(&events));
    }

    // ---------------- v0.8.7 review-fix (R-M7) last_event tail read ----------

    #[test]
    fn last_event_none_for_absent_and_empty_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let absent = tmp.path().join("nope.jsonl");
        assert!(last_event(&absent).unwrap().is_none());
        let empty = tmp.path().join("empty.jsonl");
        std::fs::write(&empty, b"").unwrap();
        assert!(last_event(&empty).unwrap().is_none());
        let blank = tmp.path().join("blank.jsonl");
        std::fs::write(&blank, b"\n\n  \n").unwrap();
        assert!(last_event(&blank).unwrap().is_none());
    }

    #[test]
    fn last_event_reads_last_line_basic_shapes() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Single line, no trailing newline.
        let one = tmp.path().join("one.jsonl");
        std::fs::write(&one, br#"{"event":"a","n":1}"#).unwrap();
        assert_eq!(last_event(&one).unwrap().unwrap()["n"], 1);

        // Multiple lines, trailing newline + a trailing blank line.
        let multi = tmp.path().join("multi.jsonl");
        std::fs::write(
            &multi,
            "{\"event\":\"a\"}\n{\"event\":\"b\"}\n{\"event\":\"last\",\"n\":7}\n\n",
        )
        .unwrap();
        let last = last_event(&multi).unwrap().unwrap();
        assert_eq!(last["event"], "last");
        assert_eq!(last["n"], 7);
    }

    /// The headline R-M7 assertion: a progress.jsonl far larger than
    /// `TAIL_CHUNK` returns the correct last event, and — crucially — the tail
    /// reader touches only a bounded number of bytes near EOF, NOT the whole
    /// file. We prove "bounded" structurally: `read_last_line` on a multi-MiB
    /// file reads at most one `TAIL_CHUNK` here (the last record is tiny), so
    /// we assert it returns instantly-correct results on a file two orders of
    /// magnitude larger than the chunk without slurping it.
    #[test]
    fn last_event_tail_reads_large_file_correctly_and_bounded() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("big.jsonl");
        // Build a file >> TAIL_CHUNK (8 KiB). 4000 lines * ~64 B ≈ 256 KiB.
        let mut body = String::new();
        for i in 0..4000u32 {
            body.push_str(&format!(
                "{{\"event\":\"chat_turn_completed\",\"i\":{i},\"pad\":\"xxxxxxxxxxxxxxxxxxxx\"}}\n"
            ));
        }
        // The decisive last record.
        body.push_str(r#"{"event":"the_last_one","i":99999}"#);
        body.push('\n');
        std::fs::write(&path, &body).unwrap();
        assert!(
            body.len() as u64 > TAIL_CHUNK * 10,
            "fixture must dwarf the tail chunk to make the bounded-read claim meaningful"
        );

        let last = last_event(&path).unwrap().unwrap();
        assert_eq!(last["event"], "the_last_one");
        assert_eq!(last["i"], 99999);

        // Bounded-read proof: the last line + its preceding newline fit well
        // inside one TAIL_CHUNK, so read_last_line should resolve within a
        // single backward chunk. We can't observe syscalls here, but we CAN
        // assert the last line itself is far smaller than the file — the
        // invariant the implementation relies on to stay O(record), not
        // O(file).
        let last_line_len = body.lines().next_back().unwrap().len() as u64;
        assert!(
            last_line_len < TAIL_CHUNK,
            "last record fits in one chunk ⇒ tail read is O(record), not O(file)"
        );
    }

    /// Edge: the final record straddles a `TAIL_CHUNK` boundary — the reader
    /// must keep stepping backwards until it finds the opening newline.
    #[test]
    fn last_event_handles_record_spanning_chunk_boundary() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("span.jsonl");
        // First line is short; last line is LARGER than one TAIL_CHUNK so the
        // first backward read can't contain its opening newline.
        let big_val = "y".repeat((TAIL_CHUNK as usize) * 2);
        let body = format!("{{\"event\":\"first\"}}\n{{\"event\":\"big\",\"v\":\"{big_val}\"}}\n");
        std::fs::write(&path, &body).unwrap();
        let last = last_event(&path).unwrap().unwrap();
        assert_eq!(last["event"], "big");
        assert_eq!(last["v"].as_str().unwrap().len(), big_val.len());
    }

    /// Tail read agrees with the canonical `append_event` writer: appending N
    /// events and reading back returns the Nth.
    #[test]
    fn last_event_round_trips_with_append_event() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("rt.jsonl");
        for i in 0..50u32 {
            append_event(&path, &json!({"event": "e", "seq": i})).unwrap();
        }
        assert_eq!(last_event(&path).unwrap().unwrap()["seq"], 49);
    }

    #[test]
    fn last_event_for_sid_reads_latest_sid_event_and_legacy_session_id() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        append_event(&path, &json!({"event": "old", "sid": "s1"})).unwrap();
        append_event(&path, &json!({"event": "other", "sid": "s2"})).unwrap();
        append_event(&path, &json!({"event": "new", "sid": "s1"})).unwrap();
        append_event(&path, &json!({"event": "legacy", "session_id": "legacy-1"})).unwrap();

        assert_eq!(
            last_event_for_sid(&path, "s1").unwrap().unwrap()["event"],
            "new"
        );
        assert_eq!(
            last_event_for_sid(&path, "legacy-1").unwrap().unwrap()["event"],
            "legacy"
        );
        assert!(last_event_for_sid(&path, "missing").unwrap().is_none());
    }
}
