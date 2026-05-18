---
name: ccteam-advise
description: "Claude + Codex parallel advisor for architecture decisions, algorithm trade-offs, and security / performance second opinions. Use when the user says '/ccteam-advise <NL>', 'second opinion on X', 'should I pick A or B', 'ask both Claude and Codex about X', or otherwise asks for a cross-vendor consult. V0.6.0 Wave 3 (F112 §A)."
---

# /ccteam-advise — Claude + Codex parallel voting

V0.6.0 Wave 3 — F112 §A. Single user-facing scenario: a hard
question that benefits from two independent opinions. The skill
fans the same question to **Claude** (via a `general-purpose`
subagent) and **Codex** (via `codex exec --json` on PATH), then
synthesises a verdict the user can act on without reading both
transcripts in full.

## V0.6.0 skill family (you are here)

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

## Pre-flight: is Codex available?

Before spawning the parallel advisors:

```bash
codex --version 2>/dev/null && codex login status 2>/dev/null
```

- Both succeed → run the full dual-vendor dispatch (§Step 1–§Step 2)
- Either fails → fall back to **Claude-only single advisor**, print
  a one-line note: `Codex unavailable (reason); running Claude-only
  advisor.` Then dispatch the single Task and skip the verdict
  synthesis (just print Claude's answer).

**Test override**: when `$CCTEAM_CODEX_BIN` is set, treat that path
as the codex binary instead of probing PATH. Lets unit / e2e tests
inject a fake codex without modifying `$PATH`.

## Step 1: Parallel dispatch

Issue **two tool calls in the same assistant turn** so they run
concurrently:

1. Claude advisor:

   ```
   Task({
     subagent_type: "general-purpose",
     description: "Claude advisor on <topic-short>",
     prompt: "<full question + context>\n\nReturn: (a) your
              recommendation, (b) the 2-3 strongest reasons, (c) any
              dealbreaker risks. Keep total response under 250 words."
   })
   ```

2. Codex advisor (via Bash since the Task tool can't target
   Codex directly):

   ```bash
   CODEX_BIN="${CCTEAM_CODEX_BIN:-codex}"
   "$CODEX_BIN" exec --json --skip-git-repo-check <<'PROMPT'
   <full question + context>

   Return: (a) your recommendation, (b) the 2-3 strongest reasons,
   (c) any dealbreaker risks. Keep total response under 250 words.
   PROMPT
   ```

   Parse the JSONL output, take the final assistant message body.
   On non-zero exit / parse failure, treat as `Codex error: <stderr
   one-liner>` and continue with Claude-only verdict synthesis.

If the daemon is running and exposes `ccteam__advise_vote` via MCP,
the skill MAY use that tool instead of the manual fan-out — same
result with daemon-side cost accounting. Detection: a previous turn
in this session must have seen `mcp__ccteam__advise_vote` registered;
otherwise stick to the direct dispatch.

## Step 2: Verdict synthesis

After both advisors return, produce ONE final verdict in the
prescribed format. Do not echo either advisor's raw reply verbatim
— summarise.

```
ADVISOR VERDICT
===============
Claude: <100-word summary of Claude's recommendation + key reasons>
Codex:  <100-word summary of Codex's recommendation + key reasons>

合成:    <150-word final recommendation>
分歧度:  <0-5>   # 0 = both agree, 5 = full opposition

Notes (optional):
  - <any dealbreaker either vendor raised>
  - <any caveat the user should be aware of>
```

Synthesis rules:

- **Both agree** → "Both agree: <X>. Reasons: ..." Pick the
  recommendation as the verdict.
- **Conflict** → "Claude says <X> because <r1>; Codex says <Y>
  because <r2>. Recommended: <Z> because <why>." If the conflict
  is irreducible (e.g. matter of taste), say so explicitly and let
  the user pick.
- **One vendor strongly objects** (e.g. dealbreaker risk) → that
  vendor's veto carries unless the user explicitly overrides.

Don't pad the verdict — under 250 words total including all the
sections above.

## Output language

Match the user's input language. If the user wrote in Chinese, the
verdict labels stay as `Claude:` / `Codex:` / `合成:` / `分歧度:`
(those are already mixed). If the user wrote in English, swap
`合成` → `Synthesis`, `分歧度` → `Divergence`.

## Cost / budget hint

Each `/ccteam-advise` invocation is roughly:

- 1 Claude Task (Sonnet 4.5, ~10k context, ~1k output) ≈ $0.04
- 1 Codex `exec` (gpt-5-codex, ~10k context, ~1k output) ≈ $0.05

Total ≈ $0.10 per consult. If the user calls `/ccteam-advise`
> 20× in one session, surface a soft reminder:
"You've asked the parallel advisor 20+ times; consider using
`/ccteam-team 3:reviewer` for an extended debate instead — same
cost, deeper exploration."

## What this skill does NOT do

- **No iterative debate** — single round of voting + one verdict.
  Use `/ccteam-team 3:reviewer` for multi-turn debate.
- **No retention** — verdicts are session-local; nothing is written
  to `~/.ccteam/`.
- **No tool execution** — advisors return text only; the skill
  never lets either vendor write files or run shell commands on
  behalf of the user.
- **No 3+ vendor fan-out** — for N-way voting use the MCP tool
  `ccteam__advise_parallel` instead (Wave 3 daemon).

## Red lines

- **Never run the two advisors sequentially** — defeats the point
  of parallel voting. Single assistant turn with both tool calls.
- **Never quote an entire advisor reply** — synthesise. The user
  asked for a verdict, not two essays.
- **Never silently fall through to Claude-only** — if Codex is
  unavailable, print the one-line `Codex unavailable: <reason>`
  marker before the verdict.
- **Never invoke `/ccteam-advise` recursively** — if mid-synthesis
  you find you need more facts, ask the user, don't fan out again.

## Where to look in the repo

- `@docs/v0-6-0/prd.md` §F112 §A — skill design SoT
- `@crates/ccteam-cli/src/mcp_advise_tools.rs` — MCP tools the
  daemon registers (`ccteam__advise_vote` / `ccteam__advise_parallel`)
- `@crates/ccteam-core/src/execution/codex_exec.rs` — the
  CodexExecAdapter the daemon route uses for the same primitive
- `@CLAUDE.md` §三 — architectural red lines
