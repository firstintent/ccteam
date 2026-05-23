---
name: ccteam-advise
description: "Claude + Codex parallel advisor for architecture decisions, algorithm trade-offs, and security / performance second opinions. Use when the user says '/ccteam-advise <NL>', 'second opinion on X', 'should I pick A or B', 'ask both Claude and Codex about X', or otherwise asks for a cross-vendor consult. Backed by the daemon MCP tools `mcp__ccteam__advise_vote` / `mcp__ccteam__advise_parallel` (V0.6.5 F152 + F153)."
---

# /ccteam-advise — Claude + Codex parallel voting

V0.6.5 F152 + F153 — daemon-backed real implementations of
`mcp__ccteam__advise_vote` (Claude + Codex one-shot + synthesised
verdict via a third Claude call) and `mcp__ccteam__advise_parallel`
(N-of-N raw answers, no synthesis). Single user-facing scenario: a
hard question that benefits from two independent opinions, fanned to
both vendors in parallel and reduced to a 3-5 sentence verdict.

## V0.6.5 skill family (you are here)

| User intent | Skill |
|---|---|
| Top-level NL dispatcher | `ccteam` |
| Spin up a short-lived team in the current session | `ccteam-team` |
| Start a new project / workflow / IM bot | `ccteam-creator` |
| Manage existing ccteam projects | `ccteam-control` |
| One-shot IM token onboarding | `ccteam-im-setup` |
| **Codex + Claude parallel advisor (this skill)** | **`ccteam-advise`** |

## When to invoke

Trigger phrases (LLM matching — no regex needed):

- `/ccteam-advise <question>` — explicit slash entry
- `/ccteam-advise vote <question>` — same, routes to `mcp__ccteam__advise_vote`
- `/ccteam-advise parallel <question>` — routes to `mcp__ccteam__advise_parallel`
  (N raw answers, no verdict; user wants to read both rather than a synthesis)
- "second opinion on X"
- "ask both Claude and Codex about X"
- "should I pick A or B" / "X vs Y trade-off"
- "verify this security review" / "double-check this perf claim"

**Don't** invoke for:

- Single-vendor work (use the Task tool with `general-purpose`)
- Implementation work (use `/ccteam-team`)
- Project creation (use `/ccteam-creator`)

If the user's question is < 1 sentence ("which is better?"), ask
one clarifying question before the dispatch — vague prompts produce
generic answers from both vendors.

## Step 1: Pick the route (vote vs parallel)

| Intent | Route | Output shape |
|---|---|---|
| "Give me one consolidated recommendation" | `mcp__ccteam__advise_vote` | `verdict` (3-5 sentence prose) + both raw answers + `agreement` (agree / partial / disagree / unknown) |
| "Give me both answers, I'll decide" | `mcp__ccteam__advise_parallel` | `answers: [{vendor, answer, status}, ...]` (N rows, no synth) |
| Default (ambiguous user intent) | `mcp__ccteam__advise_vote` | synthesised verdict is the safer default |

Both tools enforce the same rolling 24h budget cap in
`<ccteam_root>/cost-budget.json` (default 0.50 USD/24h, override via
`max_cost_usd`). Over-cap → tool returns `ok:false, error:"budget_exceeded"`
without spawning any advisor; surface that to the user verbatim.

## Step 2: Invoke the MCP tool

Preferred path — one MCP tool call per consult:

```json
// advise_vote
{
  "name": "mcp__ccteam__advise_vote",
  "arguments": {
    "question": "Should the orchestrator gate writes via inotify or a polling watcher?",
    "context": "Target: 200ms latency cap; cross-platform (linux + macOS).",
    "codex_timeout_secs": 60,
    "max_cost_usd": 0.50
  }
}
```

```json
// advise_parallel
{
  "name": "mcp__ccteam__advise_parallel",
  "arguments": {
    "question": "Should the orchestrator gate writes via inotify or a polling watcher?",
    "n": 4,
    "vendors": ["claude", "codex"],
    "timeout_secs": 60
  }
}
```

The daemon spawns both vendors in parallel (`claude -p ...` + `codex
exec --json -`), aggregates the result, and returns a single MCP
response. Cost is accounted into `<ccteam_root>/cost-budget.json`
on the daemon side — no client-side bookkeeping needed.

### Codex unavailable

When the codex binary is missing from `$PATH` (or
`$CCTEAM_CODEX_BIN` points at a non-executable file), the daemon
returns the result with the Codex slot marked
`status:"unavailable"` and the verdict (advise_vote) explicitly
includes the line `Codex unavailable: <reason>`. The call is still
`ok:true`. Surface the unavailability note to the user verbatim —
never silently fall through to a "Claude-only" output without
flagging it.

### Daemon not running

If `mcp__ccteam__advise_vote` is not registered in this session (no
daemon running, or daemon-side `CCTEAM_DISABLE_TOOLS=advise`), fall
back to the **manual fan-out** below.

## Step 3: Render the verdict

For `advise_vote`, the daemon already returns a synthesised
`verdict` string. Render it as-is, then optionally show the two raw
answers under collapsible headings if the user asks ("show me both
sides").

For `advise_parallel`, render each `answers[i]` row as its own
section — do not synthesise yourself.

### Example output (advise_vote)

User: `/ccteam-advise vote should I write the new persistence layer in Rust or Go for a high-throughput message queue?`

Daemon returns (paraphrased shape — actual JSON fields are
`verdict` / `claude_answer` / `codex_answer` / `agreement` / `budget`):

```
ADVISOR VERDICT (agreement: agree)
==================================
Both Claude and Codex recommend Rust for a high-throughput message
queue, citing zero-cost abstractions, predictable latency under
load, and Tokio's mature async runtime. The lone Codex caveat is
team familiarity — if your team has shipped 3+ Go services already
and zero Rust, the migration cost can dominate the latency win for
the first 6 months. Recommended: Rust if you have at least one
Rust-comfortable engineer; otherwise Go with a `sync.Pool`-heavy
hot path is acceptable for ~50% of Rust's throughput ceiling.

Claude's answer:    [collapsed — expand with "show Claude's reply"]
Codex's answer:     [collapsed — expand with "show Codex's reply"]

Budget: 0.015 / 0.500 USD used today.
```

Synthesis rules (the daemon already follows these for
`advise_vote`; replicate them only if you fall back to manual mode):

- **Both agree** → "Both agree: <X>. Reasons: ..." Pick the
  recommendation as the verdict.
- **Conflict** → "Claude says <X> because <r1>; Codex says <Y>
  because <r2>. Recommended: <Z> because <why>." If the conflict
  is irreducible (e.g. matter of taste), say so explicitly and let
  the user pick.
- **One vendor strongly objects** (e.g. dealbreaker risk) → that
  vendor's veto carries unless the user explicitly overrides.

Don't pad the verdict — under 250 words total.

## Output language

Match the user's input language. If the user wrote in Chinese, the
verdict labels stay as `Claude:` / `Codex:` / `合成:` / `分歧度:`
(those are already mixed). If the user wrote in English, swap
`合成` → `Synthesis`, `分歧度` → `Divergence`.

## Cost / budget hint

Each `mcp__ccteam__advise_vote` call:

- 1 Claude advisor call (`claude -p ...`, ~250-word cap) ≈ $0.005
- 1 Codex `exec` call (`codex exec --json -`, same cap) ≈ $0.005
- 1 Claude verdict synth call ≈ $0.005

Total ≈ $0.015 per `advise_vote` consult; `advise_parallel` is
`N × $0.005` (no synth step). The daemon enforces a default rolling
24h cap of $0.50 (≈ 33 vote consults / 100 parallel slots).

If the user calls `/ccteam-advise` > 20× in one session, surface a
soft reminder: "You've asked the parallel advisor 20+ times;
consider using `/ccteam-team 3:reviewer` for an extended debate
instead — same cost, deeper exploration."

## Manual fallback (no daemon)

If the daemon isn't running or the advise group is disabled, issue
**two tool calls in the same assistant turn** so they run
concurrently:

1. Claude advisor (via the Task tool):

   ```
   Task({
     subagent_type: "general-purpose",
     description: "Claude advisor on <topic-short>",
     prompt: "<full question + context>\n\nReturn: (a) your
              recommendation, (b) the 2-3 strongest reasons, (c) any
              dealbreaker risks. Keep total response under 250 words."
   })
   ```

2. Codex advisor (via Bash):

   ```bash
   CODEX_BIN="${CCTEAM_CODEX_BIN:-codex}"
   "$CODEX_BIN" exec --json --skip-git-repo-check <<'PROMPT'
   <full question + context>

   Return: (a) your recommendation, (b) the 2-3 strongest reasons,
   (c) any dealbreaker risks. Keep total response under 250 words.
   PROMPT
   ```

   Parse the JSONL output, take the final `agent_message` body.
   On non-zero exit / parse failure, treat as `Codex error: <stderr
   one-liner>` and continue with a Claude-only verdict synthesis.

Pre-flight Codex availability check:

```bash
codex --version 2>/dev/null && codex login status 2>/dev/null
```

- Both succeed → run the full dual-vendor dispatch
- Either fails → fall back to **Claude-only single advisor**, print
  a one-line note: `Codex unavailable (reason); running Claude-only
  advisor.` Then dispatch the single Task and skip the verdict
  synthesis (just print Claude's answer).

**Test override**: when `$CCTEAM_CODEX_BIN` is set, both the daemon
adapter and the manual fallback treat that path as the codex binary
instead of probing `$PATH`. Lets unit / e2e tests inject a fake
codex without modifying `$PATH`.

## What this skill does NOT do

- **No iterative debate** — single round of voting + one verdict.
  Use `/ccteam-team 3:reviewer` for multi-turn debate.
- **No retention** — verdicts are session-local; only the cost
  ledger persists to `<ccteam_root>/cost-budget.json`.
- **No tool execution** — advisors return text only; the skill
  never lets either vendor write files or run shell commands on
  behalf of the user.
- **N-way fan-out without synthesis** uses `mcp__ccteam__advise_parallel`
  with `n: 3..=8` — supported and shipped (no longer a placeholder).

## Red lines

- **Never run the two advisors sequentially** — defeats the point
  of parallel voting. The daemon already parallelises; in manual
  fallback, issue both tool calls in a single assistant turn.
- **Never quote an entire advisor reply** — synthesise. The user
  asked for a verdict, not two essays.
- **Never silently fall through to Claude-only** — if Codex is
  unavailable, print the one-line `Codex unavailable: <reason>`
  marker before the verdict (daemon path already does this).
- **Never invoke `/ccteam-advise` recursively** — if mid-synthesis
  you find you need more facts, ask the user, don't fan out again.
- **Never bypass the budget cap** — if the daemon returns
  `error:"budget_exceeded"`, surface it; do not retry with a
  silently raised `max_cost_usd`.

## Where to look in the repo

- `@docs/versions/v0-6-5/prd.md` §F152 / §F153 / §F154 — real-impl
  PRD (supersedes the V0.6.0 stub design)
- `@crates/ccteam-cli/src/mcp_advise_tools.rs` — MCP tool schemas
  + dispatchers for `ccteam__advise_vote` / `ccteam__advise_parallel`
- `@crates/ccteam-core/src/advise.rs` — vendor adapter helpers
  (`run_claude_advisor` / `run_codex_advisor` / budget ledger
  persistence)
- `@crates/ccteam-cli/tests/mcp_advise_vote_test.rs` — vote happy
  path + Codex-unavailable + budget-exceeded e2e coverage
- `@crates/ccteam-cli/tests/mcp_advise_parallel_test.rs` —
  parallel round-robin + claude-only + unavailable slot + budget
  e2e coverage
- `@CLAUDE.md` §三 — architectural red lines
