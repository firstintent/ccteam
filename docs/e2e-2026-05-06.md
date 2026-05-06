# E2E Acceptance Test Report — 2026-05-06

**Session:** `4f0aede9-b18a-4312-a6c5-19ccd53045b9`
**Duration:** ~1 hour (04:38–05:37 UTC)
**Tester:** orchestrator + manual supervision
**Scope:** M0–M3 shipped functionality

---

## Summary

| Test | Result |
|---|---|
| P0.1 install + doctor | ✅ PASS |
| P0.2 dev happy path (todo-cli) | ⚠️ PASS WITH MANUAL INTERVENTION |
| P0.3 product-research (ai) | ⚠️ PARTIAL — feasibility/verdict not reached |
| P0.4 meta-agent NL dispatch | ✅ PASS |
| P0.5 decisions queue | ⚠️ PARTIAL — no outbox events generated |
| P2 ccteam-mcp 9 tools | ✅ PASS |

**3 release blockers found (P0).** Every dev phase transition required manual intervention due to F1+F2. Product shipped successfully with manual steering; automated flow is broken.

---

## P0 Failures

### F1+F2 — SubagentStop → `/btw` → toolless side agent

**Severity:** P0. Blocks all automated dev phase transitions.

**Root cause:** `is_idle()` in `crates/ccteam-core/src/progress.rs` does not include `SubagentStop`:

```rust
// progress.rs, is_idle()
"Stop" | "notification" | "session_start" | "SessionEnd" | "phase_done" | "escalate"
// SubagentStop is NOT here
```

When a phase completes, Claude emits `Stop` then `SubagentStop` 2–5 seconds later. The orchestrator's tick runs between these two events, sees `last_event_type = SubagentStop`, determines the session is **not idle**, and injects the next phase via `/btw` instead of bare `send-keys`. `/btw` creates a toolless side-agent that cannot execute tools — the phase cannot proceed.

**Evidence from progress.jsonl (todo-cli plan-eng → implement):**

```
04:55:05  phase_done   plan-eng
04:55:05  Stop
04:55:09  SubagentStop          ← orchestrator reads this as "not idle"
04:55:17  phase_inject implement ← inject fired — but via /btw
04:55:27  SubagentStop          ← trailing SubagentStop after inject
                                                         (5:23 gap)
05:00:40  PreToolUse            ← first tool use after manual intervention
```

Gap of 5 minutes 23 seconds between phase_inject and first tool use. A normal inject-to-tool gap is 15–60 seconds (observed on test-author→test-run: 47s; implement→test-author via SessionEnd: 1:47).

The issue does NOT affect product-research kickoff/market-survey because those phases don't internally spawn Tasks. Phases that use `Task` internally (implement, test-author, test-run, fix) always generate a SubagentStop after Stop.

Sub-skills (code-reviewer) use `SessionEnd` as their terminal event, which IS in `is_idle()` — this is why test-author injection after the subskill worked cleanly.

**Reproduction:**
```
ccteam new my-project dev
# Wait for plan-eng to complete
# Observe progress.jsonl: Stop → SubagentStop (2-5s later) → phase_inject
# The inject will fire while last_event_type = SubagentStop
# Claude session will show: "このメッセージは サイドエージェント（ツールなし）として受け取っています"
```

**Fix direction:**

Option A (minimal): Add `"SubagentStop"` to `is_idle()` list.

Option B (correct): Treat SubagentStop as a signal that Claude just finished internal Task work. A SubagentStop following a `phase_done` within N seconds should be classified as idle. Consider a short debounce (e.g., wait 5s after SubagentStop before deciding idle vs busy).

**Files:** `crates/ccteam-core/src/progress.rs` (is_idle), `crates/ccteam-core/src/orchestrator.rs` (decide_tick_from_events)

---

### F6 — Fix phase `completion_signal` mismatch causes false escalation

**Severity:** P0. A project with all tests passing escalates after 3 fix iterations.

**Root cause:** `phases/06-fix.md` front matter declares `completion_signal: TESTS_GREEN`, but the phase body instructs Claude to output `PHASE_DONE: fix` when tests are green. Claude follows the textual instruction and writes `PHASE_DONE: fix`. `parse_phase_end.rs` watches for `TESTS_GREEN` and never finds it. After 3 iterations the fix loop escalates.

**Evidence:**

```
# crates/ccteam-hooks/src/parse_phase_end.rs line 72
if last_assistant_text.contains(&state.front.completion_signal) {  // watches "TESTS_GREEN"

# phases/06-fix.md (body text)
本轮全绿 → `PHASE_DONE: fix`                                       // Claude outputs this

# escalation.0.md
reason: fix-loop hit 3 iterations without TESTS_GREEN

# orch log 05:24:35
WARN project escalated slug="todo-cli-rust-sqlite-add-list-done-rm" phase=fix
     reason=fix-loop hit 3 iterations without TESTS_GREEN
```

All 37 tests were passing at escalation time (`cargo test` confirmed green in test-run phase).

**Fix direction:** Change `completion_signal` in `phases/06-fix.md` to `PHASE_DONE: fix` to match what Claude actually outputs. Alternatively, update the phase body to output `TESTS_GREEN` before `PHASE_DONE: fix`, but the former is simpler.

**Files:** `phases/06-fix.md` (completion_signal field), `crates/ccteam-hooks/src/parse_phase_end.rs` (line 72)

---

### F8 — `ccteam resume` doesn't clear terminal escalation state

**Severity:** P0. After any escalation, orchestrator is permanently blocked even after `ccteam resume`.

**Root cause:** `is_terminal_state()` in `dag.rs` checks for any `phase_history` entry with `status == "escalated"`:

```rust
// dag.rs lines 110-120
pub fn is_terminal_state(&self, state: &ProjectState) -> bool {
    for h in &state.phase_history {
        if h.status == "escalated" {
            return true;  // never cleared
        }
        ...
    }
    false
}
```

`run_resume()` in `commands.rs` archives `escalation.md`, sets `phase_state = Idle`, and clears `user_pause_pending` — but **does not modify `phase_history`**. On the next orchestrator tick, `is_terminal_state()` sees the escalated entry and emits `TickAction::NoOp` forever.

**Evidence:**

After manual resume and manual ship injection, `state.json` still shows:
```json
"phase_history": [
  ...
  {"phase": "fix", "status": "escalated", ...}
],
"current_phase": "fix",
"phase_state": "idle"
```

Orchestrator log shows no further phase advances after the escalation, even though ship completed manually.

**Fix direction:** `run_resume()` must either (a) remove the escalated entry from `phase_history`, or (b) mark it with a different status (e.g., `"resumed"`), and `is_terminal_state()` must treat `"resumed"` as non-terminal.

**Files:** `crates/ccteam-cli/src/commands.rs` (run_resume, lines 297–315), `crates/ccteam-core/src/dag.rs` (is_terminal_state, lines 110–120)

---

## P1 Observations

### F3 — Meta-agent used wrong `ccteam start` API

`rob-meta` attempted `ccteam start markdown-web-...` (positional slug argument). `ccteam start` only accepts `--foreground`; positional args are rejected. The meta-agent recovered gracefully — it caught the error, wrote the outbox reply anyway, and the project started correctly via `ccteam new`. No data loss.

**Fix direction:** `ccteam start --slug <slug>` could be added for clarity, or the meta-agent skill prompt should document the correct invocation.

---

### F7 — Stall checker rapid-fire burst

When the orchestrator processes a batch of queued events (e.g., a subskill completing + phase advancing simultaneously), the stall check fires once per event rather than once per tick. Observed at 05:30:32 when `phase advanced markdown-web implement→test-author`:

```
05:30:32.889 INFO  phase advanced markdown-web implement→test-author
05:30:32.927 WARN  stall ≥5min ai/differentiation-analysis silent_seconds=419
05:30:32.940 WARN  stall ≥5min ai/differentiation-analysis silent_seconds=419
05:30:32.952 WARN  stall ≥5min ai/differentiation-analysis silent_seconds=419
05:30:32.966 WARN  stall ≥5min ai/differentiation-analysis silent_seconds=419
05:30:32.979 WARN  stall ≥5min ai/differentiation-analysis silent_seconds=420
```

5 identical warnings emitted in 90ms. Not harmful (no action taken per stall warn at this threshold), but indicates the stall check loop runs once per event rather than being debounced per tick.

**Fix direction:** Deduplicate stall warnings per project per tick; track `last_stall_warned_at` per project and suppress if within the same orchestrator tick cycle.

---

### AskUserQuestion checkbox UX

During product-research kickoff (hybrid decision mode, round 2), submitting the AskUserQuestion multi-select dialog with no items selected returned `"Invalid tool parameters"`. Claude recovered gracefully and proceeded to the next round. Not a ccteam bug (Claude Code behavior), but worth noting for UX testing.

---

## What Worked

- **P0.1 doctor:** All 5 sub-checks passed. 8 agents linked, skill installed, MCP registered, meta-agent created.
- **Sub-skills M2:** Code-reviewer triggered automatically at `phase_done: implement` (`subskill_started` → `session_start` → `SessionEnd` → `subskill_done` → next phase inject). Output written to `.ccteam/code-review.md`. Review was substantive (10/10, all plan requirements confirmed).
- **Phase protocol (PHASE_DONE parsing, progress.jsonl appending):** All 9 phase transitions logged correctly; orchestrator state machine tracked all phases.
- **Product-research multi-phase advancement:** kickoff → market-survey → differentiation-analysis → value-proposition all advanced without /btw issues (these phases don't use Task internally).
- **AskUserQuestion hybrid decisions:** kickoff phase ran 5 clarification rounds; user answers incorporated into `brief.md`.
- **ccteam-mcp 9 tools:** All tools listed and `ccteam__ls` returns correct JSON.
- **Meta-agent NL dispatch:** Recognized build request, enriched spec, dispatched to dev team with correct team/slug.
- **Cost and context tracking:** `cost_used_usd` accumulated correctly ($6.02 todo-cli, $1.52 ai, $1.80 markdown-web). Context token tracking working.
- **Fix-loop re-injection mechanism:** Block decision with re-injected prompt confirmed working for 3 iterations before escalation.

---

## Phase Timeline (todo-cli dev run)

```
04:38:43  phase_inject plan-eng
04:55:05  phase_done   plan-eng     → advance to implement
04:55:17  phase_inject implement    [/btw issue — manual fix ~5 min]
05:07:54  phase_done   implement    → subskill code-reviewer starts
05:08:20  subskill_started implement
05:10:23  subskill_done implement   → phase_inject test-author [clean SessionEnd]
05:17:53  phase_done   test-author  → advance to test-run
05:18:04  phase_inject test-run
05:19:16  phase_done   test-run     → advance to fix
05:19:43  phase_inject fix
05:24:21  escalate     fix          [F6: TESTS_GREEN mismatch; 37/37 tests green]
            [manual: ship injected directly]
05:35:30  phase_done   ship

Total cost: $6.02  Context: 96k tokens  
Artifacts: 2 git commits, 37/37 tests green, ship-report.md, code-review.md
```

---

## Recommended Fix Sequence

Priority order for unblocking automated dev runs:

1. **F1+F2** — Add `SubagentStop` to `is_idle()` (or debounce 5s post-SubagentStop). Every dev phase transition hits this. One-line fix.
2. **F6** — Change `completion_signal` in `phases/06-fix.md` from `TESTS_GREEN` to `PHASE_DONE: fix`. One-line fix.
3. **F8** — In `run_resume()`, update the escalated `phase_history` entry to `"resumed"`, and update `is_terminal_state()` to treat `"resumed"` as non-terminal. ~10-line fix.

After F1+F2+F6+F8 are fixed, re-run P0.2 (dev happy path) without manual intervention. Then re-run P0.3 to reach feasibility/verdict and validate PHASE_DONE_PENDING protocol.

---

## Still Untested

- **P1.1 golden_rules executor:** golden_rule violation → phase escalation. Requires adding a failing rule to a phase YAML and observing escalation.
- **P1.2 PHASE_DONE_PENDING:** feasibility/verdict phases with open decisions. Product-research project `ai` was at `value-proposition in_flight` at report time; it has not yet reached feasibility.
- **P0.5 decisions queue full path:** `ccteam decisions` command works; no outbox clarify files generated yet (product-research hasn't reached feasibility/verdict).
- **Context reset (60% threshold):** Not triggered during this run (max 96k / 600k = 16%).
- **`ccteam pause`/`ccteam resume` end-to-end:** Only tested via `ccteam resume` (affected by F8).
