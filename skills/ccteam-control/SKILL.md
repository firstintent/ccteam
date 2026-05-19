---
name: ccteam-control
description: Manage ccteam projects from any Claude Code session via the `ccteam` CLI and `mcp__ccteam__*` MCP tools. Use when the user asks about ccteam status, wants to inspect / pause / resume an active ccteam project, needs cost / progress info, or asks for advice on intervening when a project is stuck. Primary consumer is the ccteam meta-agent session; secondary consumer is the user's own daily-driver Claude.
---

# ccteam-control

ccteam is an autonomous project orchestrator built on Claude Code.
This skill makes ccteam reachable from any Claude session via the
`ccteam` CLI + `mcp__ccteam__*` MCP tool surface.

## V0.5.0 skill family (you are here)

This skill is one of three shipped V0.5.0 skills. Pick by intent:

| Intent | Skill | Quick example |
|---|---|---|
| Create a new ccteam project / scaffold workflow.yaml / scaffold agents / scaffold project-local skills | **`ccteam-creator`** | "make a new ccteam project for X" / "add a QA loop to this repo" |
| Spin up an Anthropic Agent Team in the current Claude session (no `ccteam init` needed) | **`ccteam-team`** | `/ccteam-team "fix all TS errors"` |
| **Inspect / control existing ccteam projects (this skill)** | **`ccteam-control`** | "what's the cost on todo-cli?" / "pause bookmark-mgr" |

If the user wants something this skill *doesn't* cover, point them at
the sibling skill rather than improvising. The three skills are
intentionally narrow.

## MCP server is preferred; Bash is the fallback

Once `ccteam doctor --install-mcp` has registered the MCP server, every
Claude session sees `mcp__ccteam__*` tools — call those first. The
`Bash` + `--format json` path stays as a fallback for sessions where the
MCP server isn't registered yet.

**V0.4.6 F89 CLI surface reorg.** The top-level `ccteam` CLI exposes
only user-facing commands (`init` / `start` / `stop` / `new` / `ls` /
`status` / `show` / `doctor` / `web` / `remove`). Hook handlers and
meta-agent integration points (`spawn` / `send` / `peek` / `attach` /
`progress` / `resume` / `mcp-serve` / `hook`) live under `ccteam
internal <subcmd>`. V0.5.0 dropped the legacy top-level aliases —
prefer the new path:

```bash
ccteam internal peek <slug>          # was: ccteam peek <slug>
ccteam internal progress <slug>      # was: ccteam progress <slug>
ccteam internal resume <slug>        # was: ccteam resume <slug>
ccteam internal send <slug> "<body>" # was: ccteam send <slug> "<body>"
ccteam internal spawn <slug> <role>  # was: ccteam spawn <slug> <role>
```

## Capability index — MCP first, Bash fallback

| What you want | MCP tool (preferred) | Bash fallback |
|---|---|---|
| List all projects                | `mcp__ccteam__admin_ls`                | `ccteam ls --format json` |
| One project's full state         | `mcp__ccteam__workflow_show`              | `ccteam show <slug> --format json` |
| Recent progress events           | `mcp__ccteam__workflow_progress`          | `ccteam internal progress <slug>` |
| Capture session pane content     | `mcp__ccteam__workflow_peek`              | `ccteam internal peek <slug>` |
| Start a new project (delegate)   | `mcp__ccteam__workflow_new`               | `ccteam new <slug>` (and see `ccteam-creator` skill for the dialogue) |
| Pause project (no kill)          | `mcp__ccteam__workflow_pause`             | `ccteam pause <slug>` |
| Resume project                   | `mcp__ccteam__workflow_resume`            | `ccteam internal resume <slug>` |
| Send NL to a session inbox       | `mcp__ccteam__workflow_send_to_session`   | `ccteam internal send <slug> "<body>"` (or write `.ccteam/inbox/msg-<ts>-NNN.md` directly) |
| Inject a structured decision     | `mcp__ccteam__workflow_inject_decision`   | (compose body manually + send_to_session) |
| Health checks                    | (Bash only)                      | `ccteam doctor --tool-surface` |
| Install meta-agent               | (Bash only)                      | `ccteam doctor --install-meta-agent` |

When the MCP server is registered, `ccteam doctor --install-mcp` (run
once) wires `mcpServers.ccteam` into `~/.claude.json`. Existing Claude
sessions need `/reload-mcp`; new sessions pick it up immediately.

## Typical workflows

### A) Cross-project status report

```bash
ccteam ls --format json | jq '.projects[] | {slug, cost_used_usd, age_seconds, stall_level}'
```

Then narrate the table to the user — call out anything in
`stall_level: "warn"` or higher.

### B) Pre-launch clarification

When the user says "make a todo cli" but the brief is ambiguous,
**delegate to `ccteam-creator`** rather than asking yourself. The creator
skill is the dialogue specialist (step 1/2/3/4); this skill is the
control / status specialist.

### C) Stuck-project triage

```bash
ccteam show <slug> --format json | jq '{cost: .state.cost_used_usd, fix_count: .state.auto_loop_cycle_count, recent: .recent_events[-5:]}'
ccteam internal peek <slug>
```

Combine the two outputs and recommend exactly **one** of:
- `ccteam internal attach <slug>` — user wants to drive
- `ccteam pause <slug>` — pause and think
- `ccteam internal resume <slug>` after fixing — back to autonomous

## Decision principles

- **Attach vs peek vs pause** — `attach` if the user will drive in
  real-time, `peek` if they only want to look, `pause` if they want
  ccteam to stop dispatching while they decide.
- **One question at a time** — clarification turns must surface a
  single question. Don't batch.
- **Don't show `progress.jsonl` raw** — it's noisy. Summarize key
  events (agent spawn / done, escalations, ship events) in NL.

## What this skill cannot do

- It cannot `attach` for the user — `tmux attach` is a TTY interaction;
  the user must run it in their own terminal.
- It cannot edit `<project>/.ccteam/` metadata directly. All
  control flows through the CLI (or `~/.ccteam/control/` files for
  asynchronous control signals).
- It cannot start the ccteam orchestrator daemon — that's `ccteam
  start` and is an ops decision, not an agent decision.

## Meta-agent specifics

When this skill is loaded inside the ccteam meta-agent session:

1. Prefer `mcp__ccteam__*` over Bash. The meta-agent role prompt covers
   the dispatcher-not-worker rule — this skill is the tool list the
   dispatcher uses.
2. After every status reply, write an outbox file at
   `~/projects/meta/.ccteam/outbox/reply-<ts>-<seq>.md` per
   `docs/interfaces.md` §3.4.3.
3. When the user has resolved a project's clarify / escalation, use
   `mcp__ccteam__workflow_inject_decision` (or its Bash equivalent) to push
   the resolution back into the project session — it constructs a
   structured decision payload (interfaces §4.1.1) and atomically
   writes it to the project's inbox so the orchestrator delivers it
   on the next tick.
