---
name: ccteam-control
description: |
  Manage ccteam projects from any Claude Code session. Use when the
  user asks about ccteam status, wants to start a new ccteam project,
  needs to inspect / pause / resume an active ccteam project, or asks
  for advice on intervening when a project is stuck. Primary consumer
  is the ccteam meta-agent session; secondary consumer is the user's
  own daily-driver claude.
allowed-tools: [Bash]
---

# ccteam-control

ccteam is an autonomous project orchestrator built on Claude Code.
This skill makes ccteam reachable from any claude session via the
`ccteam` CLI. Default to `--format json` so output is structured.

## Capability index

| What you want | Command |
|---|---|
| List all projects                | `ccteam ls --format json` |
| One project's full state         | `ccteam show <slug> --format json` |
| Recent progress events           | `ccteam progress <slug>` (or `--tail` for live stream) |
| Capture session pane content     | `ccteam peek <slug>` |
| Start a new project              | `ccteam new --team=dev "<request>"` |
| Pause project (no kill)          | `ccteam pause <slug>` *(M2 implements; M1 stub)* |
| Resume project                   | `ccteam resume <slug>` |
| Reject project                   | `ccteam reject <slug>` *(M2 implements; M1 stub)* |
| Health checks                    | `ccteam doctor --tool-surface` |
| Install meta-agent for a user    | `ccteam doctor --install-meta-agent <handle>` |

`--format json` is on every query command (`ls`, `show`). Prefer JSON
when piping output into your own analysis. The schema lives in
`docs/interfaces.md` §10.3.

## Typical workflows

### A) Cross-project status report

```bash
ccteam ls --format json | jq '.projects[] | {slug, current_phase, phase_state, cost_used_usd, age_seconds}'
```

Then narrate the table to the user — call out anything in
`stall_level: "warn"` or higher.

### B) Pre-launch clarification

When the user says "make a todo cli" but the brief is ambiguous,
**ask one clarifying question** before dispatch:

> Web app or CLI? Local-only or cloud-sync? Tech-stack preference?

Pick the single most blocking question. Only after they answer, run:

```bash
ccteam new --team=dev "<refined brief>"
```

### C) Stuck-project triage

```bash
ccteam show <slug> --format json | jq '{phase: .state.current_phase, fix_count: .state.fix_cycle_count, recent: .recent_events[-5:]}'
ccteam peek <slug>
```

Combine the two outputs and recommend exactly **one** of:
- `ccteam attach <slug>` — user wants to drive
- `ccteam pause <slug>` — pause and think
- `ccteam resume <slug>` after fixing — back to autonomous

## Decision principles

- **Attach vs peek vs pause** — `attach` if the user will drive in
  real-time, `peek` if they only want to look, `pause` if they want
  ccteam to stop dispatching while they decide.
- **One question at a time** — clarification phases must surface a
  single question. Don't batch.
- **Don't show progress.jsonl raw** — it's noisy. Summarize milestones
  (phase transitions, escalations, ship events) in NL.

## What this skill cannot do

- It cannot `attach` for the user — `tmux attach` is a TTY interaction;
  the user must run it in their own terminal.
- It cannot edit `~/projects/<slug>/.ccteam/` metadata directly. All
  control flows through the CLI (or `~/.ccteam/control/` files for
  M2+ asynchronous control).
- It cannot start the ccteam orchestrator daemon — that's `ccteam
  start --foreground` and is an ops decision, not an agent decision.

## Meta-agent specifics

When this skill is loaded inside a ccteam meta-agent session:

1. Prefer `ccteam new` dispatch over doing the work yourself. The
   meta-agent role prompt (CLAUDE.md) covers the dispatcher-not-worker
   rule — this skill is the tool list the dispatcher uses.
2. After every dispatch / status reply, write an outbox file at
   `~/projects/<user>-meta/.ccteam/outbox/reply-<ts>-<seq>.md` per
   `docs/interfaces.md` §3.4.3.
3. M2.8 will swap shell-parsing for the `ccteam-mcp` MCP server. When
   that lands, prefer `mcp__ccteam__*` tools over `Bash` invocations
   of the same CLI. The `--format json` paths remain a stable fallback.
