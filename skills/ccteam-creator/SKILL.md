---
name: ccteam-creator
description: Design and scaffold a ccteam workflow inside an existing project. Use when the user says "做个 workflow" / "建一个 agent / skill" / "把 X 自动化" / "迁移 X 到 ccteam" / "给这个项目加 ccteam 流水线" / "design a multi-agent loop". Walks the user through picking agent topology, writes `workflow.yaml`, generates per-role `.claude/agents/<role>.md` with valid frontmatter, and optionally adds project-local skills. Defers to the official `agent-creator` and `skill-creator` specs for frontmatter rules — does not duplicate them.
---

# ccteam-creator

You design **content** for an existing ccteam project — workflow, agents,
and optionally project-local skills. `ccteam init` / `ccteam new` creates
the project shell; this skill fills in orchestration semantics.

You enforce three contracts. **Each contract lives in an authoritative
file — read it directly, don't paraphrase it here:**

| Contract | Authoritative spec — read with `@<path>` or `Read` |
|---|---|
| ccteam `workflow.yaml` schema | `@crates/ccteam-core/src/workflow.rs` (parser SoT) + `@docs/v0-4-0/prd.md` (§6) + `@docs/interfaces.md` |
| Claude Code **agent** frontmatter | `@~/.claude/plugins/marketplaces/claude-plugins-official/plugins/plugin-dev/agents/agent-creator.md` |
| Claude Code **skill** frontmatter | `@~/.claude/plugins/marketplaces/anthropic-agent-skills/skills/skill-creator/SKILL.md` |

If any path is missing on the host, fall back to existing in-repo
examples (e.g. `skills/ccteam-team-author/SKILL.md` for skill shape; any
`.claude/agents/<role>.md` from a deployed project for agent shape).

---

## Capability index

| What you want | Where |
|---|---|
| Pick agents + triggers | [Phase B](#phase-b--design-topology) |
| Author `workflow.yaml` | [Workflow.yaml — ccteam-specific](#workflowyaml--ccteam-specific) below |
| Author `.claude/agents/<role>.md` | `@…/plugin-dev/agents/agent-creator.md` |
| Author `.claude/skills/<name>/SKILL.md` | `@…/anthropic-agent-skills/skills/skill-creator/SKILL.md` |
| Wire artifact dirs + gitignore | [Phase D](#phase-d--wire-up) |
| Verify everything loads | [Phase E](#phase-e--verify) |

---

## Phase A — Capture intent

Information density check on the user's brief.

- **Brief is dense** (≥ 2 sentences specifying loop semantics) → skip A.
- **Single-token brief** ("做个 dex-ui 的自动化") → ask **one**
  `AskUserQuestion` with 3–4 options + Other slot. Typical archetypes:

  ```
  - 事件驱动闭环 (planner → explorer → fixer → master)
  - 单 agent 巡检 (schedule 触发,一个 agent 做完)
  - review 链 (writer → reviewer → shipper)
  - data 管线 (collector → processor → emitter)
  ```

Capture before moving on:

1. **Project root** (cwd or `--in <path>`).
2. **Agent list** — roles + 1-line responsibility each.
3. **Trigger story** — when does each agent wake?
4. **State storage** — `.ccteam/<artifact>/` local files (default per
   CLAUDE.md §三) vs external (GH / Linear / Slack — opt-in only).

Don't proceed without a single-sentence flow description.

---

## Phase B — Design topology

For each role pick exactly one trigger and a parallelism cap.

Triggers (full grammar in `@crates/ccteam-core/src/workflow.rs::parse_trigger`):
- `manual` — user invokes via `ccteam spawn <slug> <role>`
- `schedule` — interval-based (V0.4.1+ scheduler; V0.4.0 = manual placeholder)
- `gate` — waits for `trigger_gate` MCP call
- `watch:<rel/path/>` — spawn on file change under `<path>` relative to project root

Decision flow per agent:

1. Runs *every time* an upstream artifact appears? → `watch:`
2. Runs *periodically* regardless of state? → `schedule`
3. Waits for explicit user permission (e.g. ship gate)? → `gate`
4. Otherwise → `manual`.

`parallelism > 1` is **only** legal with `watch:` (validate() rejects
otherwise). Leave it unset for the other three.

### ⚠️ Self-excitation pitfall (`watch:` triggers)

A `watch:<dir>/` trigger fires on **every** filesystem event under
`<dir>/` — including `Modify` events emitted by `notify` whenever a
file inside is rewritten (e.g. `jq '.field=X' f.json > f.tmp && mv
f.tmp f.json`). If the agent's own body mutates files inside its
*own* watched dir, you get an infinite loop: agent runs → updates an
artifact's status → watcher fires → spawns the agent again.

**Real incident** (dex-ui 2026-05-16): explorer's `.ccteam/backlog/`
watch self-excited via Step 6 (status updates) + Step 7 (new
scenarios). Burst climbed from 1/min to 8/min over 4 h, 45 successful
spawns + 80 stale-spawn errors, $1.10 burned. fixer + master had the
same issue on `issues/` and `prs/`.

**Pattern to apply** (when an agent must mutate the artifact source):

| Role of dir | Watched? | Who writes |
|---|---|---|
| tracking artifacts (mutable state) | NO | the agent itself |
| trigger markers (write-once tiny JSON) | YES | upstream agent / human |

So split the QA loop's `backlog/` (tracking, unwatched) from a
new `explore-requests/` (markers, watched). Upstream writes a marker
file like `planner-<ts>.json` containing `{requested_by, at,
reason}`. The downstream agent reads + immediately archives the
marker to `<dir>.archived/` (outside the watched tree) before doing
any real work — so a re-fire on the same marker cannot happen.

Rule of thumb: **an agent must never write a file into its own
watched dir**, even transiently. If it has to leave a status
breadcrumb, put it in a sibling unwatched dir.

### Common topologies

| Topology | Agents + triggers |
|---|---|
| **QA loop** | planner:manual; explorer:watch:.ccteam/explore-requests/; fixer:watch:.ccteam/fix-requests/ (parallelism:2); master:watch:.ccteam/merge-requests/ |
| **Single watchdog** | watchdog:schedule (interval:"2h") |
| **Review chain** | writer:manual; reviewer:watch:.ccteam/drafts/; shipper:gate |
| **Data pipeline** | collector:schedule; processor:watch:input/; emitter:watch:processed/ |

The QA loop row above pairs each `watch:<dir>` with a separate
tracking dir (`backlog/` / `issues/` / `prs/`) that the corresponding
agent mutates freely. See the dex-ui workflow.yaml for a worked
example with the marker payload shape.

---

## Workflow.yaml — ccteam-specific

`workflow.yaml` is ccteam's contract (not Claude Code's). Schema is
defined by `WorkflowSpec` / `AgentSpec` in
`@crates/ccteam-core/src/workflow.rs` — when in doubt, read that file.
Minimal shape:

```yaml
name: <kebab-case>                  # required; appears in progress.jsonl
description: |                      # optional; reader-facing only
  One paragraph.

agents:                             # required, ≥ 1 entry
  <role>:                           # role = filename of .claude/agents/<role>.md
    executor: claude                # default; or `codex` (only if the
                                    # agent body is written for Codex CLI)
    trigger: <see Phase B>          # required
    parallelism: 1                  # optional; > 1 only when trigger=watch:
    input: <rel/path>               # optional; passed as $CCTEAM_INPUT
    output: <rel/path>              # optional; passed as $CCTEAM_OUTPUT
    interval: "1h"                  # only for trigger=schedule (V0.4.1+)
    timeout: "30m"                  # optional; per-session wall clock
```

### Path to write
`<project>/.ccteam/workflow.yaml` (V0.4.6 F83+ canonical) — orchestrator
also accepts the legacy `<project>/workflow.yaml` (V0.4.0–V0.4.5
fallback, removed in V0.5). New workflows go in `.ccteam/`; the
orchestrator's discovery order prefers `.ccteam/` when both are
present.

### Validation
Daemon parses on rescan (10s tick). On load you should see in the daemon
log:

```
starting project event loop slug="<slug>"
watch registered slug="<slug>" role="<role>" watch="<path>"
```

Validation errors surface as `WARN`/`ERROR` lines around the same time;
project gets skipped if invalid.

---

## Agent + skill files — defer to official specs

The frontmatter rules for `.claude/agents/<role>.md` and
`.claude/skills/<name>/SKILL.md` are **not ccteam's contracts** — they
belong to Claude Code. Don't reinvent them here.

When writing or editing an agent, read:

```
@~/.claude/plugins/marketplaces/claude-plugins-official/plugins/plugin-dev/agents/agent-creator.md
```

For skill files, read:

```
@~/.claude/plugins/marketplaces/anthropic-agent-skills/skills/skill-creator/SKILL.md
```

V0.4.0+ agents do NOT need to emit `PHASE_DONE` / `ESCALATE` sigils —
the phase machinery was deleted and agents are now triggered + observed
via `workflow.yaml` + the `ArtifactWatcher` reading `.ccteam/<dir>/*`
artifact files. End-of-session is just the normal claude `--bg` job
exit; the orchestrator writes the `agent_done` event into
`progress.jsonl` from `~/.claude/jobs/<id>/state.json`.

In-repo examples for the agent body style you can copy structurally
(not verbatim — keep them ccteam-flavored): any deployed project's
`.claude/agents/<role>.md` from a recent install, plus the default
starter at `@agents/explorer.md`.

---

## Phase D — Wire up

After writing `workflow.yaml` + agents (+ optional skills):

1. **Create artifact dirs** for every `watch:<path>/` so inotify has
   something to watch even before the loop runs:

   ```bash
   mkdir -p .ccteam/{backlog,issues,prs,acceptance}/   # adjust to your topology
   ```

2. **`.gitignore`** — orchestration state is local, not source:

   ```bash
   grep -q '^\.ccteam/' .gitignore 2>/dev/null \
     || echo -e '\n# ccteam orchestration state (local-only)\n.ccteam/' >> .gitignore
   ```

3. **Project config** — if the workflow needs settings (GH owner/repo,
   staging URL, etc.), write `.ccteam/config.json` and have agents read
   via `jq -r '.<field>' .ccteam/config.json`. Convention, not enforced.

4. **Secrets** — `.env` at project root (gitignored); agents source
   with `set -a; . .env; set +a`.

---

## Phase E — Verify

Three checks before declaring done:

1. **Daemon picks up the workflow** — within 10s of writing
   `workflow.yaml`, daemon log shows `starting project event loop` and
   `watch registered` lines for each role. If only "no workflow.yaml;
   skipping" → check file path.

2. **Agents load in Claude Code** — open a fresh `claude` session in the
   project, press `←` or run `/agents`. Each agent appears with its
   description. Missing → frontmatter problem (most common: missing
   `---` delimiters or `name` ≠ filename).

3. **Skills load** (if any) — `/skills` lists active skills. Same drill.

If all three pass: loop starts on the first artifact write (watch
triggers) or `ccteam spawn <slug> <role>` (manual).

---

## Anti-patterns

- **`workflow.yaml` under `.claude/`** — that's Claude Code's scope.
  ccteam's workflow lives at project root (or `.ccteam/`).
- **Agent without frontmatter** — invisible to Claude Code's picker
  even though the file exists.
- **`parallelism: 2` on non-watch trigger** — `WorkflowSpec::validate`
  rejects; daemon skips the project.
- **Watch dir doesn't exist at load time** — inotify install fails
  silently; create dirs in Phase D before relying on triggers.
- **Auto-commit `.ccteam/`** — orchestration state, not code.
- **Reach for external systems by default** — `.ccteam/<artifact>/` is
  the canonical state store (CLAUDE.md §三 红线). External APIs only
  when the existing human surface requires it (e.g. GH PRs for code
  review — keep those).

---

## Where to look in the repo

- `@docs/v0-4-0/prd.md` §6 — workflow.yaml + artifact-trigger architecture
- `@crates/ccteam-core/src/workflow.rs` — schema source of truth
- `@docs/interfaces.md` — protocol-level field meanings
- `@CLAUDE.md` §三 — architectural red lines (file system as control plane, etc.)
