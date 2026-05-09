<!-- ccteam-managed:skill begin -->
---
name: cct-control
description: |
  Manage ccteam projects from any Claude Code session. Use when the
  user asks about ccteam status, wants to start a new ccteam project,
  needs to inspect / pause / resume an active ccteam project, or asks
  for advice on intervening when a project is stuck. Primary consumer
  is the ccteam meta-agent session; secondary consumer is the user's
  own daily-driver claude.
allowed-tools: [Bash]
---

# cct-control

ccteam is an autonomous project orchestrator built on Claude Code.
This skill makes ccteam reachable from any claude session via the
short-prefix `cct` CLI (V0.2.2 F39).

**Prefer the MCP server.** Once `cct doctor --install-mcp` has
registered the server (M2.5), every claude session sees nine
`mcp__ccteam__*` tools — call those first. The `Bash` + `--format
json` path below stays as a fallback for sessions where the MCP
server isn't registered yet.

## Capability index — MCP first, Bash fallback

| What you want | MCP tool (preferred) | Bash fallback |
|---|---|---|
| List all projects                | `mcp__ccteam__ls`                | `cct ls --format json` |
| One project's full state         | `mcp__ccteam__show`              | `cct show <slug> --format json` |
| Recent progress events           | `mcp__ccteam__progress`          | `cct progress <slug>` |
| Capture session pane content     | `mcp__ccteam__peek`              | `cct peek <slug>` |
| Start a new dev project          | `mcp__ccteam__new`               | `cct new --team=dev "<request>"` |
| Start a product-research project | `mcp__ccteam__new`               | `cct new --team=product-research "<idea>"` |
| Pause project (no kill)          | `mcp__ccteam__pause`             | `cct pause <slug>` |
| Resume project                   | `mcp__ccteam__resume`            | `cct resume <slug>` |
| Send NL to a session inbox       | `mcp__ccteam__send_to_session`   | (write `.ccteam/inbox/msg-<ts>-NNN.md`) |
| Inject ESCALATE-style decision   | `mcp__ccteam__inject_decision`   | (compose body manually + send_to_session) |
| Health checks                    | (Bash only)                      | `cct doctor --tool-surface` |
| Install meta-agent               | (Bash only)                      | `cct doctor --install-meta-agent <handle>` |

When the MCP server is registered, `cct doctor --install-mcp` (run
once) wires `mcpServers.ccteam` into `~/.claude.json`. Existing claude
sessions need `/reload-mcp`; new sessions pick it up immediately. The
MCP server name stays `ccteam` (V0.2.2 §8.3 — namespace change is
deferred to V0.3 to avoid touching user `~/.claude.json` files).

## Typical workflows

### A) Cross-project status report

```bash
cct ls --format json | jq '.projects[] | {slug, current_phase, phase_state, cost_used_usd, age_seconds}'
```

Then narrate the table to the user — call out anything in
`stall_level: "warn"` or higher.

### B) Team selection (M3+)

ccteam now ships two teams: `dev` (build the thing) and
`product-research` (decide whether the thing is worth building).
Pick by reading the user's intent:

| Signal | Team |
|---|---|
| User wants code, brief is concrete | `dev` |
| User is unsure if the idea is worth doing / wants market validation | `product-research` |
| Brief is one or two words ("做个 todo") | Ask one disambiguating question first |

product-research is cheap (hours, not days), produces
`verdict.md` + `rationale.md` + `next-steps.md`, and may auto-suggest
spawning a follow-on dev project if PASS / CONCERN.

### C) Pre-launch clarification

When the user says "make a todo cli" but the brief is ambiguous,
**ask one clarifying question** before dispatch:

> Web app or CLI? Local-only or cloud-sync? Tech-stack preference?

Pick the single most blocking question. Only after they answer, run:

```bash
cct new --team=dev "<refined brief>"
# or, if the user still seems uncertain:
cct new --team=product-research "<idea>"
```

### D) Stuck-project triage

```bash
cct show <slug> --format json | jq '{phase: .state.current_phase, fix_count: .state.auto_loop_cycle_count, recent: .recent_events[-5:]}'
cct peek <slug>
```

Combine the two outputs and recommend exactly **one** of:
- `cct attach <slug>` — user wants to drive
- `cct pause <slug>` — pause and think
- `cct resume <slug>` after fixing — back to autonomous

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
- It cannot start the ccteam orchestrator daemon — that's `cct
  start --foreground` and is an ops decision, not an agent decision.

## Meta-agent specifics

When this skill is loaded inside a ccteam meta-agent session:

1. Prefer `mcp__ccteam__new` dispatch over doing the work yourself.
   The meta-agent role prompt (CLAUDE.md) covers the
   dispatcher-not-worker rule — this skill is the tool list the
   dispatcher uses.
2. After every dispatch / status reply, write an outbox file at
   `~/projects/<user>-meta/.ccteam/outbox/reply-<ts>-<seq>.md` per
   `docs/interfaces.md` §3.4.3.
3. When the user has resolved a project's clarify/escalation, use
   `mcp__ccteam__inject_decision` (or its Bash equivalent) to push
   the resolution back into the project session — it constructs an
   ESCALATE-style markdown payload (interfaces §4.1.1) and atomically
   writes it to the project's inbox so the orchestrator delivers it
   on the next tick.
<!-- ccteam-managed:skill end -->
