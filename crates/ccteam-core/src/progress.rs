//! progress.jsonl reader / writer + idle detection.
//!
//! progress.jsonl is the orchestrator's only state-truth source
//! (`docs/tech-design.md` §5.5). This module gives both the hook
//! handlers and the orchestrator a single set of primitives so the
//! file format and idle semantics stay in sync.

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;

/// Append `event` as one JSONL line. Creates parent dir + file when
/// missing. POSIX `O_APPEND` is atomic for sub-PIPE_BUF writes (4 KiB
/// on Linux), which our compact event lines comfortably fit under, so
/// concurrent hook + orchestrator writers don't interleave.
pub fn append_event(path: &Path, event: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create {}", parent.display()))?;
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
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?;
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
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?;
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

/// Walk `events` in reverse and return the most recent `phase_done` /
/// `escalate` event whose `phase` (for phase_done) matches `current` —
/// stopping if we hit a `phase_inject` for `current` first (anything
/// older than that injection belongs to a previous round of the same
/// phase, e.g. after a context reset).
///
/// This is needed because Claude Code emits `Stop` *and* `SubagentStop`
/// after a turn finishes. If `parse-phase-end` writes `phase_done` and
/// then a downstream hook appends `SubagentStop`, the *last* event
/// is no longer `phase_done` — but the phase did finish. Reading just
/// the last line under-reports completions.
pub fn latest_terminal_event_for_phase<'a>(
    events: &'a [Value],
    current: &str,
) -> Option<&'a Value> {
    for event in events.iter().rev() {
        let kind = event.get("event").and_then(|s| s.as_str()).unwrap_or("");
        match kind {
            "phase_done" | "phase_done_pending" => {
                let phase = event.get("phase").and_then(|s| s.as_str()).unwrap_or("");
                if phase == current {
                    return Some(event);
                }
            }
            "escalate" => return Some(event),
            "phase_inject" => {
                let phase = event.get("phase").and_then(|s| s.as_str()).unwrap_or("");
                if phase == current {
                    // Anything before this inject is a stale terminal
                    // from a prior round of the same phase.
                    return None;
                }
            }
            _ => {}
        }
    }
    None
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
    )
}

/// Build the canonical phase-injection prompt (tech-design §4.4 / §6.9).
///
/// **V0.2 M0.18 (legacy shim)**: prefer
/// [`build_phase_prompt_for_template`] which takes a `&PhaseTemplate`
/// and composes inject directives off frontmatter. This name-only
/// helper is retained for tests / call sites that don't yet have a
/// loaded template; internally it synthesizes a minimal `PhaseTemplate`
/// and runs the same composition path so the protocol literal lives
/// in exactly one place.
pub fn build_phase_prompt(phase: &str) -> String {
    let synthesized = synthesize_minimal_template(phase);
    build_phase_prompt_for_template(&synthesized, &[])
}

fn synthesize_minimal_template(phase: &str) -> crate::phases::PhaseTemplate {
    let yaml = format!("name: {phase}\nparallelism: solo\n");
    let src = format!("---\n{yaml}---\nbody\n");
    crate::phases::PhaseTemplate::parse(&src)
        .expect("synthesize_minimal_template emits a valid frontmatter")
}

/// V0.2 M0.18: phase-injection prompt composed off the frontmatter of
/// `template`. This is the **single source of truth** for protocol-
/// level directives (PHASE_DONE / ESCALATE / outbox / required IO);
/// phase markdown bodies must contain only domain content. See
/// `docs/v0-2/phase-prompt-architecture.md` §5 for the segment list and
/// §8 invariants.
///
/// `attachments` are project-relative paths to surface as `@<path>`
/// references — typically previous phase's sub-skill outputs (e.g.
/// `.ccteam/code-review.md`). The template-derived inject prompt is
/// kept ≤ 1 KB so a single `tmux send-keys` always fits (property-
/// tested by `inject_prompt_under_one_kib`).
pub fn build_phase_prompt_for_template(
    template: &crate::phases::PhaseTemplate,
    attachments: &[&str],
) -> String {
    build_phase_prompt_for_template_with_team(template, attachments, &[])
}

/// V0.2 M0.18: same as `build_phase_prompt_for_template` but also
/// appends `team.yaml.golden_rules.protocol.*` rules whose
/// `enforce: prompt_directive` — those land verbatim in the inject
/// prompt as hard constraints the assistant must honor.
pub fn build_phase_prompt_for_template_with_team(
    template: &crate::phases::PhaseTemplate,
    attachments: &[&str],
    protocol_directives: &[&str],
) -> String {
    let directives = template.effective_inject_directives();
    let has = |name: &str| directives.iter().any(|d| d == name);
    let phase = template.name.as_str();

    let mut out = String::with_capacity(512);
    // Layer 1 — every phase points the assistant at the markdown body.
    out.push_str(&format!("请按 @.ccteam/phases/{phase}.md 完成本阶段。\n"));

    if has("read_inputs") && !template.required_inputs.is_empty() {
        let refs: Vec<String> = template
            .required_inputs
            .iter()
            .map(|p| format!("@{p}"))
            .collect();
        out.push_str("\n上游产物(必读):");
        out.push_str(&refs.join(" "));
        out.push('\n');
    }

    if has("write_outputs") && !template.required_outputs.is_empty() {
        let paths = template.required_outputs.join(", ");
        out.push_str("完成时必产出:");
        out.push_str(&paths);
        out.push('\n');
    }

    if has("completion_signal") {
        out.push_str(&format!(
            "完成后:输出一行 {}\n",
            template.effective_completion_signal()
        ));
    }

    if has("escalate_grammar") {
        let dialect = template.effective_escalate_grammar_ref();
        if dialect == DEFAULT_ESCALATE_GRAMMAR_REF {
            out.push_str(
                "异常时:输出 ESCALATE: <一句话原因>(team.yaml 注册的前缀可用)\n",
            );
        } else {
            out.push_str(&format!(
                "异常时:按 {dialect} grammar 输出 ESCALATE: <prefix> <reason>\n"
            ));
        }
    }

    if has("outbox_protocol") {
        let proto = template.effective_outbox_question_protocol();
        out.push_str(&format!(
            "询问用户:写 .ccteam/outbox/clarify-<ts>.md(protocol {proto},不要用 AskUserQuestion / 纯文本)\n"
        ));
    }

    if has("auto_loop") && template.auto_loop {
        out.push_str(&format!(
            "auto_loop=true:本 phase 反复重喂 prompt 直到出现 `{}`,撞 {} 次自动 ESCALATE。\n",
            template.effective_completion_signal(),
            template.auto_loop_max_iterations,
        ));
    }

    if has("decision_mode") {
        let line = match template.decision_mode {
            crate::phases::DecisionMode::Sync => Some("decision_mode=sync:用户在场,可在本 phase 用 AskUserQuestion 直接问"),
            crate::phases::DecisionMode::Async => Some("decision_mode=async:用户离线,所有澄清走 outbox"),
            crate::phases::DecisionMode::Hybrid => None,
        };
        if let Some(line) = line {
            out.push_str(line);
            out.push('\n');
        }
    }

    if !protocol_directives.is_empty() {
        out.push_str("\n协议红线:\n");
        for d in protocol_directives {
            out.push_str("- ");
            out.push_str(d);
            out.push('\n');
        }
    }

    if !attachments.is_empty() {
        let refs: Vec<String> = attachments.iter().map(|p| format!("@{p}")).collect();
        out.push_str("\n上轮 sub-skill 产出可参考: ");
        out.push_str(&refs.join(" "));
    }

    // Trim trailing newline noise so the property-test 1 KB cap is
    // measured against a tight string.
    while out.ends_with('\n') {
        out.pop();
    }
    out
}

const DEFAULT_ESCALATE_GRAMMAR_REF: &str = crate::phases::DEFAULT_ESCALATE_GRAMMAR_REF;

/// M2.1 (V0.2 M0.18 deprecated wrapper): use
/// [`build_phase_prompt_for_template`] when a `&PhaseTemplate` is in
/// scope so the inject prompt picks up frontmatter-driven segments.
/// This name-only wrapper falls back to the minimal shape and exists
/// for legacy callers that only have a phase name (tests, the
/// auto-loop bootstrap path that pre-dates the template-aware builder).
pub fn build_phase_prompt_with_attachments(phase: &str, attachments: &[&str]) -> String {
    let mut out = build_phase_prompt(phase);
    if !attachments.is_empty() {
        let refs: Vec<String> = attachments
            .iter()
            .map(|p| format!("@{p}"))
            .collect();
        out.push_str("\n\n上轮 sub-skill 产出可参考: ");
        out.push_str(&refs.join(" "));
    }
    out
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
        let p = build_phase_prompt("implement");
        assert_eq!(idle_aware_message(&p, true), p);
        let busy = idle_aware_message(&p, false);
        assert!(busy.starts_with("/btw "));
        assert!(busy.contains("@.ccteam/phases/implement.md"));
    }

    #[test]
    fn build_phase_prompt_mentions_phase_done_sigil() {
        let p = build_phase_prompt("implement");
        assert!(p.contains("PHASE_DONE: implement"));
        assert!(p.contains("ESCALATE"));
    }

    // ---------------- V0.2 M0.18 inject-prompt template ----------------

    fn parse_template(yaml: &str) -> crate::phases::PhaseTemplate {
        let src = format!("---\n{yaml}\n---\nbody\n");
        crate::phases::PhaseTemplate::parse(&src).unwrap()
    }

    #[test]
    fn inject_prompt_for_template_always_references_phase_markdown() {
        let t = parse_template("name: implement\nparallelism: solo");
        let prompt = build_phase_prompt_for_template(&t, &[]);
        assert!(
            prompt.contains("@.ccteam/phases/implement.md"),
            "got: {prompt}"
        );
    }

    #[test]
    fn inject_prompt_includes_completion_signal_default() {
        let t = parse_template("name: implement\nparallelism: solo");
        let prompt = build_phase_prompt_for_template(&t, &[]);
        assert!(prompt.contains("PHASE_DONE: implement"));
    }

    #[test]
    fn inject_prompt_uses_explicit_completion_signal() {
        let t = parse_template(
            "name: fix\nparallelism: solo\nauto_loop: true\ncompletion_signal: TESTS_GREEN",
        );
        let prompt = build_phase_prompt_for_template(&t, &[]);
        assert!(prompt.contains("TESTS_GREEN"), "got: {prompt}");
    }

    #[test]
    fn inject_prompt_renders_required_inputs_as_at_refs() {
        let t = parse_template(
            "name: x\nparallelism: solo\nrequired_inputs:\n  - .ccteam/spec.md\n  - .ccteam/plan-eng.md",
        );
        let prompt = build_phase_prompt_for_template(&t, &[]);
        assert!(prompt.contains("@.ccteam/spec.md"));
        assert!(prompt.contains("@.ccteam/plan-eng.md"));
    }

    #[test]
    fn inject_prompt_renders_required_outputs() {
        let t = parse_template(
            "name: x\nparallelism: solo\nrequired_outputs:\n  - .ccteam/x-report.md",
        );
        let prompt = build_phase_prompt_for_template(&t, &[]);
        assert!(prompt.contains(".ccteam/x-report.md"));
    }

    #[test]
    fn inject_prompt_renders_outbox_protocol_segment() {
        let t = parse_template("name: x\nparallelism: solo");
        let prompt = build_phase_prompt_for_template(&t, &[]);
        assert!(prompt.contains("outbox"), "got: {prompt}");
        assert!(prompt.contains("AskUserQuestion"));
    }

    #[test]
    fn inject_prompt_appends_attachments_block_when_present() {
        let t = parse_template("name: x\nparallelism: solo");
        let prompt = build_phase_prompt_for_template(&t, &[".ccteam/code-review.md"]);
        assert!(prompt.contains("@.ccteam/code-review.md"));
    }

    #[test]
    fn inject_prompt_appends_team_protocol_directives() {
        let t = parse_template("name: x\nparallelism: solo");
        let prompt = build_phase_prompt_for_template_with_team(
            &t,
            &[],
            &["询问用户唯一合法出口是 outbox"],
        );
        assert!(prompt.contains("询问用户唯一合法出口是 outbox"));
        assert!(prompt.contains("协议红线"));
    }

    #[test]
    fn inject_prompt_omits_segments_when_directives_empty() {
        let t = parse_template(
            "name: x\nparallelism: solo\nrequired_inputs:\n  - .ccteam/spec.md\ninject_directives: []",
        );
        let prompt = build_phase_prompt_for_template(&t, &[]);
        // Even with required_inputs declared, an empty inject_directives
        // list opts the phase out of every conditional segment.
        assert!(!prompt.contains("@.ccteam/spec.md"));
        assert!(!prompt.contains("PHASE_DONE"));
    }

    #[test]
    fn inject_prompt_under_one_kib_for_realistic_template() {
        // Property-style: a fully-loaded V0.2 phase yaml (every
        // optional field declared, multiple inputs / outputs, plus
        // a couple of team-level protocol directives) must still fit
        // in 1 KB so a single tmux send-keys works.
        let t = parse_template(
            "name: implement\nparallelism: solo\nauto_loop: true\nauto_loop_max_iterations: 3\ncompletion_signal: 'PHASE_DONE: implement'\nrequired_inputs:\n  - .ccteam/spec.md\n  - .ccteam/plan-eng.md\n  - .ccteam/architecture.md\nrequired_outputs:\n  - .ccteam/implement-report.md\n  - .ccteam/code-review.md\ndecision_mode: async\nescalate_grammar_ref: standard\noutbox_question_protocol: v1",
        );
        let directives = [
            "询问用户唯一合法出口是 outbox,禁用 AskUserQuestion / 纯文本",
            "测试不过不算完成",
            "PR 控制在 500 行以内",
        ];
        let prompt = build_phase_prompt_for_template_with_team(
            &t,
            &[".ccteam/code-review.md"],
            &directives,
        );
        assert!(
            prompt.len() <= 1024,
            "inject prompt too long ({} bytes): {prompt}",
            prompt.len(),
        );
    }

    #[test]
    fn inject_prompt_auto_loop_segment_emitted_only_when_auto_loop_true() {
        // V0.2 M0.19: `auto_loop` defaults to `true`, so an "off"
        // template must opt out explicitly.
        let t_off = parse_template("name: x\nparallelism: solo\nauto_loop: false");
        let off = build_phase_prompt_for_template(&t_off, &[]);
        assert!(!off.contains("auto_loop=true"));

        let t_on = parse_template(
            "name: fix\nparallelism: solo\nauto_loop: true\ncompletion_signal: 'PHASE_DONE: fix'",
        );
        let on = build_phase_prompt_for_template(&t_on, &[]);
        assert!(on.contains("auto_loop=true"), "got: {on}");
    }

    #[test]
    fn inject_prompt_decision_mode_hybrid_emits_no_dedicated_line() {
        let t = parse_template("name: x\nparallelism: solo");
        let prompt = build_phase_prompt_for_template(&t, &[]);
        assert!(!prompt.contains("decision_mode=sync"));
        assert!(!prompt.contains("decision_mode=async"));
    }

    #[test]
    fn inject_prompt_decision_mode_async_surfaces_outbox_routing_hint() {
        let t = parse_template("name: x\nparallelism: solo\ndecision_mode: async");
        let prompt = build_phase_prompt_for_template(&t, &[]);
        assert!(prompt.contains("decision_mode=async"));
    }

    #[test]
    fn latest_terminal_event_finds_phase_done_when_subagent_stop_is_last() {
        // Real Claude Code 2.x event order: parse-phase-end appends
        // phase_done, then SubagentStop fires later. Naive last-event
        // lookup returns SubagentStop and the orchestrator never
        // advances. This helper must walk back past SubagentStop.
        let events = vec![
            json!({"event": "phase_inject", "phase": "plan-eng"}),
            json!({"event": "PostToolUse", "tool": "Write"}),
            json!({"event": "Stop"}),
            json!({"event": "phase_done", "phase": "plan-eng"}),
            json!({"event": "SubagentStop"}),
        ];
        let found = latest_terminal_event_for_phase(&events, "plan-eng").unwrap();
        assert_eq!(found["event"], "phase_done");
        assert_eq!(found["phase"], "plan-eng");
    }

    #[test]
    fn latest_terminal_event_stops_at_phase_inject_for_same_phase() {
        // After a context reset for plan-eng, an old phase_done from
        // the prior round should not be picked up — re-injection draws
        // a new boundary.
        let events = vec![
            json!({"event": "phase_done", "phase": "plan-eng"}),
            json!({"event": "phase_inject", "phase": "plan-eng"}),
            json!({"event": "PreToolUse"}),
        ];
        assert!(latest_terminal_event_for_phase(&events, "plan-eng").is_none());
    }

    #[test]
    fn latest_terminal_event_picks_escalate_over_irrelevant_events() {
        let events = vec![
            json!({"event": "phase_inject", "phase": "fix"}),
            json!({"event": "escalate", "reason": "fix-loop cap"}),
            json!({"event": "Stop"}),
        ];
        let e = latest_terminal_event_for_phase(&events, "fix").unwrap();
        assert_eq!(e["event"], "escalate");
    }

    #[test]
    fn latest_terminal_event_skips_phase_done_for_other_phase() {
        let events = vec![
            json!({"event": "phase_inject", "phase": "implement"}),
            json!({"event": "phase_done", "phase": "plan-eng"}), // stale
        ];
        assert!(latest_terminal_event_for_phase(&events, "implement").is_none());
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
        let events = [
            pretool_task(),
            pretool_task(),
            subagent_stop(),
        ];
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

    #[test]
    fn subagent_active_extra_subagent_stops_do_not_underflow() {
        // Defensive: stray SubagentStop events with no matching open
        // window must not panic / wrap around.
        let events = [
            subagent_stop(),
            subagent_stop(),
            pretool_task(),
        ];
        assert!(subagent_active(&events));
    }
}
